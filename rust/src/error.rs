//! Error types.
//!
//! The outcomes of a write are kept apart on purpose:
//!
//! * [`Error::Rejected`]: the exchange explicitly refused the action and
//!   nothing executed. The request is final; fix the cause and, only then,
//!   create a new request with a new nonce.
//! * [`Error::RateLimited`]: a read was throttled and the retry budget ran out.
//! * [`Error::UnknownOutcome`]: the action may or may not have executed.
//!   It carries the nonce and the exact signed bytes. Reconcile it; **never**
//!   sign the same intent again with a new nonce.

use std::fmt;
use std::time::Duration;

use crate::receipt::LegStatus;
use crate::signing::SignedRequest;

/// Execution code: the cumulative request quota is exhausted.
pub const CODE_QUOTA_EXHAUSTED: u32 = 1800;
/// Execution code: a `reserveRequestWeight` purchase would overflow the
/// purchased total.
pub const CODE_PURCHASE_OVERFLOW: u32 = 1801;
/// The text every quota rejection contains (`Too many cumulative requests
/// sent (...) for cumulative volume traded $...`). Match it as a substring: it
/// may be wrapped in `action rejected by execution (code 1800) (...)` and a
/// cancel leg appends ` asset=N`.
pub const QUOTA_REJECTION_TEXT: &str = "Too many cumulative requests sent";

/// True if `text` is a quota rejection: a per-leg error, or the text of a
/// whole-action rejection. A quota rejection is final (nothing executed for
/// that leg or action). Back off (one request per 10 seconds is still
/// accepted once over the cap), trade to free up requests, or buy requests
/// with [`ReserveRequestWeight`](crate::actions::ReserveRequestWeight); the
/// SDK never retries or buys on its own.
pub fn is_quota_rejection(text: &str) -> bool {
    text.contains(QUOTA_REJECTION_TEXT)
}

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything this crate can report.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The exchange explicitly rejected the action; nothing executed.
    #[error("action rejected: {0}")]
    Rejected(Box<Rejected>),

    /// The server answered 429 and retries ran out.
    #[error("rate limited{}", fmt_retry_after(.retry_after))]
    RateLimited {
        /// The `Retry-After` the server asked for, if it sent one.
        retry_after: Option<Duration>,
    },

    /// A write whose outcome is not known. Keep it pending and reconcile.
    #[error("outcome unknown ({}); reconcile nonce {} instead of resending", .0.reason, .0.request.nonce())]
    UnknownOutcome(Box<UnknownOutcome>),

    /// The caller supplied something that cannot be sent.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A read returned a non-success HTTP status.
    #[error("HTTP {status}: {body}")]
    Http {
        /// The status code.
        status: u16,
        /// The (truncated) response body.
        body: String,
    },

    /// A read could not reach the server or the connection failed.
    #[error("transport error: {0}")]
    Transport(String),

    /// A reply could not be decoded.
    #[error("could not decode reply: {0}")]
    Decode(String),

    /// The bin-ws login was refused or its result was inconsistent.
    #[error("login failed: {0}")]
    Login(String),

    /// The bin-ws session is closed or not connected.
    #[error("bin-ws session closed: {0}")]
    Closed(String),

    /// A local order book can no longer follow its stream; resubscribe.
    #[error("order book out of sync: {0}")]
    Book(String),
}

impl Error {
    /// True for [`Error::UnknownOutcome`].
    pub fn is_unknown_outcome(&self) -> bool {
        matches!(self, Error::UnknownOutcome(_))
    }

    /// The unknown outcome, if this is one.
    pub fn as_unknown_outcome(&self) -> Option<&UnknownOutcome> {
        match self {
            Error::UnknownOutcome(u) => Some(u),
            _ => None,
        }
    }

    /// The rejection, if this is one.
    pub fn as_rejected(&self) -> Option<&Rejected> {
        match self {
            Error::Rejected(r) => Some(r),
            _ => None,
        }
    }

    /// True for a whole-action rejection because the request quota is
    /// exhausted (see [`Rejected::is_quota_exhausted`]). Per-leg quota errors
    /// are in the receipt: see [`Receipt::quota_rejected_legs`](crate::Receipt::quota_rejected_legs).
    pub fn is_quota_exhausted(&self) -> bool {
        self.as_rejected().is_some_and(Rejected::is_quota_exhausted)
    }
}

fn fmt_retry_after(retry_after: &Option<Duration>) -> String {
    match retry_after {
        Some(d) => format!(" (retry after {}s)", d.as_secs_f64()),
        None => String::new(),
    }
}

/// An explicit rejection of a whole action. Nothing executed.
#[derive(Debug, Clone)]
pub struct Rejected {
    /// The execution-layer code, for example 91, 203, 204 or 1160. Code 205
    /// (duplicate nonce) is never reported here: the original request may have
    /// executed, so it is an [`UnknownOutcome`].
    pub code: Option<u32>,
    /// The server's message.
    pub message: String,
    /// Per-leg errors when the rejection came as `statuses[]`.
    pub legs: Vec<LegStatus>,
    /// The raw reply body.
    pub raw: String,
}

impl Rejected {
    /// True if the request quota is exhausted: code 1800, or the message (or a
    /// leg error) contains `Too many cumulative requests sent`. The rejection
    /// is final. The original text stays in [`message`](Self::message).
    pub fn is_quota_exhausted(&self) -> bool {
        self.code == Some(CODE_QUOTA_EXHAUSTED)
            || is_quota_rejection(&self.message)
            || self
                .legs
                .iter()
                .any(|l| matches!(l, LegStatus::Error(e) if is_quota_rejection(e)))
    }

    /// True if a `reserveRequestWeight` purchase was refused because it would
    /// overflow the purchased total (code 1801).
    pub fn is_purchase_overflow(&self) -> bool {
        self.code == Some(CODE_PURCHASE_OVERFLOW)
    }
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.code {
            Some(code) => write!(f, "code {code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Why an outcome is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnknownReason {
    /// No reply within the timeout.
    Timeout,
    /// The connection ended before a reply arrived.
    Disconnected(String),
    /// The request failed before or while being written; it may or may not
    /// have reached the server.
    SendFailed(String),
    /// A non-200 HTTP status. HTTP status alone is not proof of non-execution.
    HttpStatus(u16),
    /// The gateway answered 429. Honour `retry_after`; the action may still
    /// have reached the node.
    RateLimited {
        /// The `Retry-After` the server asked for, if it sent one.
        retry_after: Option<Duration>,
    },
    /// The reply was not valid JSON or not shaped like an exchange reply.
    MalformedReply,
    /// The reply is valid but does not prove a final outcome: mixed or
    /// incomplete `statuses[]`, `committedResult`, `localCommitted`, a
    /// query-before-retry hint, a leg count that does not match, a duplicate
    /// nonce (code 205) or `action already pending`.
    AmbiguousReceipt,
}

impl fmt::Display for UnknownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnknownReason::Timeout => f.write_str("timeout"),
            UnknownReason::Disconnected(m) => write!(f, "disconnected: {m}"),
            UnknownReason::SendFailed(m) => write!(f, "send failed: {m}"),
            UnknownReason::HttpStatus(s) => write!(f, "HTTP {s}"),
            UnknownReason::RateLimited { .. } => f.write_str("HTTP 429"),
            UnknownReason::MalformedReply => f.write_str("malformed reply"),
            UnknownReason::AmbiguousReceipt => f.write_str("ambiguous receipt"),
        }
    }
}

/// A write whose outcome is unknown.
///
/// It owns what is needed to reconcile: the signed request (nonce, the exact
/// bytes that were sent, and the oids/cloids the action referred to) and
/// whatever the server said. The same bytes may be submitted again with
/// [`ExchangeClient::submit`](crate::ExchangeClient::submit): a duplicate nonce
/// cannot execute twice. Signing the same intent again with a new nonce can
/// double-execute, so this crate never does it.
#[derive(Debug, Clone)]
pub struct UnknownOutcome {
    /// The request as signed and sent.
    pub request: SignedRequest,
    /// Why the outcome is unknown.
    pub reason: UnknownReason,
    /// Legs observed in a partial or ambiguous reply, if any.
    pub legs: Vec<LegStatus>,
    /// The raw reply body, if one arrived.
    pub reply: Option<String>,
}

impl UnknownOutcome {
    /// The nonce of the original request.
    pub fn nonce(&self) -> u64 {
        self.request.nonce()
    }
}
