//! Transports: how signed actions and info queries reach the exchange.
//!
//! REST is the default and the way to send orders: `POST /exchange`. A
//! logged-in [`BinWsSession`](crate::BinWsSession) also implements both traits,
//! but the public `/bin-ws` endpoint does not accept `action_json` (the server
//! replies `504 Gateway Timeout`); it serves market data, order and fill events
//! and `info_json` queries. The bin-ws action transport is kept for endpoints
//! that do accept it.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::RETRY_AFTER;

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
    /// The request was provably never written, so it cannot have executed.
    /// Only a transport that can prove this reports it.
    NotSent(String),
    /// A non-success HTTP status.
    Status {
        /// The status code.
        status: u16,
        /// The (truncated) body.
        body: String,
        /// The parsed `Retry-After`, if any.
        retry_after: Option<Duration>,
    },
    /// An error reply on bin-ws (`{"type":"error"}`), for example `429 ...`.
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

/// Carries signed `/exchange` request bodies.
///
/// Implementations must send the bytes unchanged and return the body of the
/// exchange's answer.
pub trait ActionTransport: Send + Sync {
    /// Sends one signed request body.
    fn post_action<'a>(&'a self, body: &'a [u8]) -> BoxFuture<'a, Result<Vec<u8>, TransportError>>;
}

/// Carries `/info` queries and returns the same body REST would return.
pub trait InfoTransport: Send + Sync {
    /// Sends one query body and returns the data of the answer.
    fn post_info<'a>(&'a self, body: &'a [u8]) -> BoxFuture<'a, Result<Vec<u8>, TransportError>>;
}

const MAX_BODY: usize = 16 << 20;

/// The REST side: one HTTP client and a base URL.
#[derive(Clone, Debug)]
pub(crate) struct Rest {
    http: reqwest::Client,
    base: String,
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
        })
    }

    /// POSTs `body` unchanged to `<base><path>`.
    pub(crate) async fn post(
        &self,
        path: &str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<RestReply, TransportError> {
        let url = format!("{}{path}", self.base);
        let response = self
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .timeout(timeout)
            .body(body.to_vec())
            .send()
            .await
            .map_err(map_reqwest)?;
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

impl ActionTransport for RestActions {
    fn post_action<'a>(&'a self, body: &'a [u8]) -> BoxFuture<'a, Result<Vec<u8>, TransportError>> {
        Box::pin(async move {
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
        })
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
