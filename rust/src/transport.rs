//! Transports: how signed actions and info queries reach the exchange.
//!
//! Orders, cancellations and modifications are sent with REST `POST /exchange`.
//! `/info` queries go over REST too, or, for the query types bin-ws accepts,
//! over a logged-in [`BinWsSession`](crate::BinWsSession) as `info_json`
//! through [`InfoTransport`].

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue, RETRY_AFTER};

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Why a request did not produce a usable reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// No reply in time.
    Timeout,
    /// The connection ended before the reply arrived.
    Disconnected(String),
    /// The request could not be written (no connection, connect failure).
    SendFailed(String),
    /// A non-success HTTP status.
    Status {
        /// The status code.
        status: u16,
        /// The (truncated) body.
        body: String,
        /// The parsed `Retry-After`, if any.
        retry_after: Option<Duration>,
    },
    /// An error reply to a bin-ws `info_json` query (`{"type":"error"}`),
    /// for example `429 ...`.
    ErrorReply(String),
    /// Anything else.
    Other(String),
}

impl TransportError {
    /// True if this is a rate-limit answer (HTTP 429 or a bin-ws `429` reply).
    pub fn is_rate_limited(&self) -> bool {
        match self {
            TransportError::Status { status, .. } => *status == 429,
            TransportError::ErrorReply(text) => text.contains("429"),
            _ => false,
        }
    }

    /// The `Retry-After` hint, if one was sent.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            TransportError::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Carries `/info` queries and returns the same body REST would return.
pub trait InfoTransport: Send + Sync {
    /// Sends one query body and returns the data of the answer.
    fn post_info<'a>(&'a self, body: &'a [u8]) -> BoxFuture<'a, Result<Vec<u8>, TransportError>>;
}

const MAX_BODY: usize = 16 << 20;

/// The header that carries the optional gateway credential.
pub(crate) const TOKEN_HEADER: HeaderName = HeaderName::from_static("x-riverbit-mm-token");

/// Validates a gateway credential for use as a header value. An empty string
/// means "no credential". The error never contains the credential.
pub(crate) fn parse_gateway_token(token: &str) -> Result<Option<HeaderValue>, crate::error::Error> {
    if token.is_empty() {
        return Ok(None);
    }
    // Visible ASCII only: no CR, LF, other control characters or non-ASCII.
    if !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(crate::error::Error::InvalidInput(
            "the gateway credential must be visible ASCII without spaces or control characters (it is sent as an HTTP header value)"
                .into(),
        ));
    }
    let mut value = HeaderValue::from_str(token).map_err(|_| {
        crate::error::Error::InvalidInput(
            "the gateway credential is not a valid HTTP header value".into(),
        )
    })?;
    value.set_sensitive(true);
    Ok(Some(value))
}

/// The REST side: one HTTP client, a base URL and an optional gateway
/// credential. The credential goes only to this base URL and never shows up
/// in `Debug` output.
#[derive(Clone)]
pub(crate) struct Rest {
    http: reqwest::Client,
    base: String,
    token: Option<HeaderValue>,
}

impl std::fmt::Debug for Rest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rest")
            .field("base", &self.base)
            .field("gateway_token", &self.token.is_some())
            .finish()
    }
}

pub(crate) struct RestReply {
    pub status: u16,
    pub body: Vec<u8>,
    pub retry_after: Option<Duration>,
}

impl Rest {
    pub(crate) fn new(
        base_url: &str,
        http: Option<reqwest::Client>,
    ) -> Result<Self, crate::error::Error> {
        let http = match http {
            Some(c) => c,
            // Never follow a redirect: a signed POST that is replayed to
            // another location has an unknown outcome. A 3xx is returned to
            // the caller as a non-200 status.
            None => reqwest::Client::builder()
                .user_agent(concat!("riverbit-mm-sdk-rust/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| crate::error::Error::Transport(format!("build HTTP client: {e}")))?,
        };
        Ok(Rest {
            http,
            base: base_url.trim_end_matches('/').to_string(),
            token: None,
        })
    }

    /// Sets (or, for an empty string, clears) the gateway credential.
    pub(crate) fn set_token(&mut self, token: &str) -> Result<(), crate::error::Error> {
        self.token = parse_gateway_token(token)?;
        Ok(())
    }

    pub(crate) fn base(&self) -> &str {
        &self.base
    }

    pub(crate) fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// POSTs `body` unchanged to `<base><path>`.
    pub(crate) async fn post(
        &self,
        path: &str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<RestReply, TransportError> {
        let url = format!("{}{path}", self.base);
        let mut request = self
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(timeout)
            .body(body.to_vec());
        if let Some(token) = &self.token {
            request = request.header(TOKEN_HEADER, token.clone());
        }
        let response = request.send().await.map_err(map_reqwest)?;
        // A caller-supplied client may follow redirects. The body is already
        // gone then; refuse to treat the answer as the answer of this URL.
        let sent_to =
            reqwest::Url::parse(&url).map_err(|e| TransportError::SendFailed(e.to_string()))?;
        if *response.url() != sent_to {
            return Err(TransportError::Other(
                "the request was redirected; the HTTP client must not follow redirects".into(),
            ));
        }
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after);
        let bytes = response.bytes().await.map_err(map_reqwest)?;
        if bytes.len() > MAX_BODY {
            return Err(TransportError::Other("response body too large".into()));
        }
        Ok(RestReply {
            status,
            body: bytes.to_vec(),
            retry_after,
        })
    }
}

fn map_reqwest(e: reqwest::Error) -> TransportError {
    if e.is_timeout() {
        TransportError::Timeout
    } else if e.is_connect() || e.is_builder() || e.is_request() && !e.is_body() {
        TransportError::SendFailed(e.without_url().to_string())
    } else {
        TransportError::Disconnected(e.without_url().to_string())
    }
}

/// `Retry-After` as a whole number of seconds. HTTP dates are not used by the
/// exchange and are ignored.
pub(crate) fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

pub(crate) fn truncate(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    text.chars().take(500).collect()
}

/// `POST /exchange` over HTTP.
pub(crate) struct RestActions {
    pub rest: Rest,
    pub timeout: Duration,
}

impl RestActions {
    /// Sends one signed request body, unchanged, and returns the body of the
    /// exchange's answer.
    pub(crate) async fn post_action(&self, body: &[u8]) -> Result<Vec<u8>, TransportError> {
        let reply = self.rest.post("/exchange", body, self.timeout).await?;
        if reply.status == 200 {
            Ok(reply.body)
        } else {
            Err(TransportError::Status {
                status: reply.status,
                body: truncate(&reply.body),
                retry_after: reply.retry_after,
            })
        }
    }
}

/// `POST /info` over HTTP.
pub(crate) struct RestInfo {
    pub rest: Rest,
    pub timeout: Duration,
}

impl InfoTransport for RestInfo {
    fn post_info<'a>(&'a self, body: &'a [u8]) -> BoxFuture<'a, Result<Vec<u8>, TransportError>> {
        Box::pin(async move {
            let reply = self.rest.post("/info", body, self.timeout).await?;
            if reply.status == 200 {
                Ok(reply.body)
            } else {
                Err(TransportError::Status {
                    status: reply.status,
                    body: truncate(&reply.body),
                    retry_after: reply.retry_after,
                })
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_token_validation() {
        assert!(parse_gateway_token("").unwrap().is_none());
        assert!(parse_gateway_token("abc.DEF-123_~").unwrap().is_some());
        for bad in [
            "a\r\nb",
            "a\nb",
            "a b",
            "tok\u{e9}n",
            "a\tb",
            "a\0b",
            "\u{7f}",
        ] {
            let err = parse_gateway_token(bad).unwrap_err().to_string();
            assert!(!err.contains(bad.trim()) || bad.trim().is_empty(), "{err}");
        }
    }

    #[test]
    fn rest_debug_hides_the_token() {
        let mut rest = Rest::new("https://example.invalid/", None).unwrap();
        rest.set_token("super-secret-credential").unwrap();
        assert!(rest.has_token());
        let shown = format!("{rest:?}");
        assert!(!shown.contains("super-secret-credential"), "{shown}");
        assert!(shown.contains("true"));
    }

    #[test]
    fn retry_after_seconds() {
        assert_eq!(parse_retry_after("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_retry_after(" 12 "), Some(Duration::from_secs(12)));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    }

    #[test]
    fn rate_limit_detection() {
        let status = TransportError::Status {
            status: 429,
            body: String::new(),
            retry_after: Some(Duration::from_secs(2)),
        };
        assert!(status.is_rate_limited());
        assert_eq!(status.retry_after(), Some(Duration::from_secs(2)));
        assert!(TransportError::ErrorReply("429 Too Many Requests".into()).is_rate_limited());
        assert!(!TransportError::ErrorReply("bad request".into()).is_rate_limited());
        assert!(!TransportError::Timeout.is_rate_limited());
    }
}
