//! Parsing of `/exchange` replies into per-leg receipts.
//!
//! HTTP 200 and even `status: "ok"` can contain per-leg errors, so every leg
//! of `statuses[]` is inspected. Whole-action rejections, per-leg rejections
//! and unknown outcomes are kept apart: only a reply that proves nothing
//! executed becomes [`Error::Rejected`]; a reply that merely fails to prove a
//! final outcome becomes [`Error::UnknownOutcome`]. A duplicate nonce (code
//! 205) and `action already pending` are unknown outcomes too: the original
//! request may have executed.

use serde_json::Value;

use crate::actions::ActionKind;
use crate::error::{Error, Rejected, UnknownOutcome, UnknownReason};
use crate::signing::SignedRequest;

/// The result of one leg of an action (`statuses[i]`).
#[derive(Clone, Debug, PartialEq)]
pub enum LegStatus {
    /// `"success"` (cancel legs).
    Success,
    /// The order rests on the book with no fills.
    Resting {
        /// The exchange order ID.
        oid: u64,
        /// The client order ID, if the reply echoes it.
        cloid: Option<String>,
    },
    /// The order filled in whole or in part. It may still have remaining open
    /// size: query `orderStatus` for the current state.
    Filled {
        /// The exchange order ID.
        oid: u64,
        /// Total filled size, a decimal string.
        total_sz: String,
        /// Average fill price, a decimal string.
        avg_px: String,
        /// The client order ID, if the reply echoes it.
        cloid: Option<String>,
    },
    /// `waitingForTrigger` or `waitingForFill`: needs tracking.
    Waiting {
        /// The wire name of the state.
        kind: String,
        /// The order ID, if present.
        oid: Option<u64>,
    },
    /// The leg was rejected; the text is the server's error.
    Error(String),
    /// A shape this crate does not know. Never treat it as success.
    Unrecognized(Value),
}

impl LegStatus {
    /// Parses one entry of `statuses[]`.
    pub fn from_value(value: &Value) -> LegStatus {
        match value {
            Value::String(s) if s == "success" => LegStatus::Success,
            Value::String(s) if s == "waitingForFill" || s == "waitingForTrigger" => {
                LegStatus::Waiting {
                    kind: s.clone(),
                    oid: None,
                }
            }
            Value::Object(map) if map.len() == 1 => {
                let (key, inner) = map.iter().next().expect("one entry");
                let oid = inner.get("oid").and_then(Value::as_u64);
                let text = |name: &str| inner.get(name).and_then(Value::as_str).map(str::to_string);
                match key.as_str() {
                    "error" => match inner.as_str() {
                        Some(e) => LegStatus::Error(e.to_string()),
                        None => LegStatus::Unrecognized(value.clone()),
                    },
                    "resting" => match oid {
                        Some(oid) => LegStatus::Resting {
                            oid,
                            cloid: text("cloid"),
                        },
                        None => LegStatus::Unrecognized(value.clone()),
                    },
                    "filled" => match (oid, text("totalSz"), text("avgPx")) {
                        (Some(oid), Some(total_sz), Some(avg_px)) => LegStatus::Filled {
                            oid,
                            total_sz,
                            avg_px,
                            cloid: text("cloid"),
                        },
                        _ => LegStatus::Unrecognized(value.clone()),
                    },
                    "waitingForFill" | "waitingForTrigger" => LegStatus::Waiting {
                        kind: key.clone(),
                        oid,
                    },
                    _ => LegStatus::Unrecognized(value.clone()),
                }
            }
            _ => LegStatus::Unrecognized(value.clone()),
        }
    }

    /// The order ID this leg reports, if any.
    pub fn oid(&self) -> Option<u64> {
        match self {
            LegStatus::Resting { oid, .. } | LegStatus::Filled { oid, .. } => Some(*oid),
            LegStatus::Waiting { oid, .. } => *oid,
            _ => None,
        }
    }

    /// True for a rejected leg.
    pub fn is_error(&self) -> bool {
        matches!(self, LegStatus::Error(_))
    }
}

/// The parsed reply to a signed action that the exchange accepted for
/// processing. Inspect [`legs`](Receipt::legs): some legs may be rejected
/// while others executed, and partial success is never rolled back.
#[derive(Clone, Debug)]
pub struct Receipt {
    /// The nonce of the request this receipt answers.
    pub nonce: u64,
    /// `statuses[]`, one per leg in request order. Empty for the bare
    /// acknowledgment of a single `modify`.
    pub legs: Vec<LegStatus>,
    /// True for the `{"type":"default"}` acknowledgment of a single `modify`:
    /// accepted, but not final and without the new oid.
    pub default_ack: bool,
    /// The raw reply body.
    pub raw: String,
}

impl Receipt {
    /// True if no leg was rejected.
    pub fn all_ok(&self) -> bool {
        !self.legs.iter().any(LegStatus::is_error)
    }

    /// The legs that were rejected, with their index and error text.
    pub fn rejected_legs(&self) -> Vec<(usize, &str)> {
        self.legs
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match l {
                LegStatus::Error(e) => Some((i, e.as_str())),
                _ => None,
            })
            .collect()
    }

    /// The order ID of each leg, in order (`None` for legs without one).
    pub fn oids(&self) -> Vec<Option<u64>> {
        self.legs.iter().map(LegStatus::oid).collect()
    }
}

/// Node diagnostics for `status: "err"` replies that may still carry effects.
const AMBIGUOUS_HINTS: [&str; 4] = [
    "query the order status",
    "waiting for its trigger",
    "not verified",
    "already pending",
];

/// Extracts `N` from `action rejected by execution (code N)`.
pub(crate) fn execution_code(text: &str) -> Option<u32> {
    let rest = text.split("action rejected by execution (code ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn is_ambiguous_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    AMBIGUOUS_HINTS.iter().any(|h| lower.contains(h))
}

/// Execution code of a duplicate nonce: a request with this nonce was already
/// seen, so the original may have executed.
const DUPLICATE_NONCE: u32 = 205;

/// True for replies that say "this nonce or action was already seen".
fn is_duplicate(text: &str) -> bool {
    execution_code(text) == Some(DUPLICATE_NONCE) || is_ambiguous_text(text)
}

/// The one documented whole-action rejection that arrives as `status: "ok"`:
/// a single `PrecisionExceeded` status for a multi-leg action.
fn precision_rejection(legs: &[LegStatus], expected: usize) -> Option<&str> {
    match legs {
        [LegStatus::Error(message)] if expected > 1 && message.contains("PrecisionExceeded") => {
            Some(message)
        }
        _ => None,
    }
}

fn statuses_of(container: &Value) -> Option<Vec<LegStatus>> {
    let array = container.get("data")?.get("statuses")?.as_array()?;
    Some(array.iter().map(LegStatus::from_value).collect())
}

fn is_default_ack(response: &Value) -> bool {
    response
        .as_object()
        .is_some_and(|m| m.len() == 1 && m.get("type").and_then(Value::as_str) == Some("default"))
}

/// Turns a reply body into a receipt, a rejection or an unknown outcome.
pub(crate) fn interpret(body: &[u8], request: &SignedRequest) -> Result<Receipt, Error> {
    let raw = String::from_utf8_lossy(body).into_owned();
    let unknown = |reason: UnknownReason, legs: Vec<LegStatus>| {
        Error::UnknownOutcome(Box::new(UnknownOutcome {
            request: request.clone(),
            reason,
            legs,
            reply: Some(raw.clone()),
        }))
    };
    let rejected = |code: Option<u32>, message: String, legs: Vec<LegStatus>| {
        Error::Rejected(Box::new(Rejected {
            code,
            message,
            legs,
            raw: raw.clone(),
        }))
    };

    let Ok(reply) = serde_json::from_slice::<Value>(body) else {
        return Err(unknown(UnknownReason::MalformedReply, vec![]));
    };
    let Some(status) = reply.get("status").and_then(Value::as_str) else {
        return Err(unknown(UnknownReason::MalformedReply, vec![]));
    };
    let response = reply.get("response").unwrap_or(&Value::Null);

    match status {
        "ok" => {
            let Some(legs) = statuses_of(response) else {
                let modify = matches!(request.kind(), ActionKind::Modify | ActionKind::BatchModify);
                if modify && is_default_ack(response) {
                    return Ok(Receipt {
                        nonce: request.nonce(),
                        legs: vec![],
                        default_ack: true,
                        raw,
                    });
                }
                return Err(unknown(UnknownReason::AmbiguousReceipt, vec![]));
            };
            if legs.iter().any(|l| matches!(l, LegStatus::Unrecognized(_))) {
                return Err(unknown(UnknownReason::AmbiguousReceipt, legs));
            }
            // A single modify has no fixed leg count on success (it answers
            // with a bare acknowledgment) but a `statuses[]` reply has one leg.
            let expected = match request.expected_legs() {
                Some(n) => Some(n),
                None if request.kind() == ActionKind::Modify => Some(1),
                None => None,
            };
            if let Some(expected) = expected {
                if legs.len() != expected {
                    // PrecisionExceeded yields one status and allocates no oid
                    // to any leg of the action. Any other length mismatch does
                    // not prove that nothing executed.
                    if let Some(message) = precision_rejection(&legs, expected) {
                        return Err(rejected(None, message.to_string(), legs));
                    }
                    return Err(unknown(UnknownReason::AmbiguousReceipt, legs));
                }
            }
            Ok(Receipt {
                nonce: request.nonce(),
                legs,
                default_ack: false,
                raw,
            })
        }
        "err" => {
            if let Some(committed) = reply.get("committedResult").filter(|c| !c.is_null()) {
                // Effects may already exist. Only a result made of plain,
                // terminal errors proves that nothing executed.
                let Some(legs) = statuses_of(committed) else {
                    return Err(unknown(UnknownReason::AmbiguousReceipt, vec![]));
                };
                let all_plain_errors = !legs.is_empty()
                    && legs
                        .iter()
                        .all(|l| matches!(l, LegStatus::Error(e) if !is_ambiguous_text(e)));
                let message = response.as_str().unwrap_or("action rejected").to_string();
                if all_plain_errors && !is_duplicate(&message) {
                    return Err(rejected(execution_code(&message), message, legs));
                }
                return Err(unknown(UnknownReason::AmbiguousReceipt, legs));
            }
            let text = match response {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            // A duplicate nonce or a pending action says the original may have
            // executed: never a rejection, whatever else the text carries.
            if text.is_empty() || is_duplicate(&text) {
                return Err(unknown(UnknownReason::AmbiguousReceipt, vec![]));
            }
            if let Some(code) = execution_code(&text) {
                return Err(rejected(Some(code), text, vec![]));
            }
            Err(rejected(None, text, vec![]))
        }
        _ => Err(unknown(UnknownReason::MalformedReply, vec![])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{
        Action, BatchModify, Cancel, CancelItem, Modify, ModifyItem, Order, Place, Tif,
    };
    use crate::signing::{ActionInfo, Signer, SigningDomain};

    fn request(action: &impl Action) -> SignedRequest {
        let signer =
            Signer::from_hex("0x0101010101010101010101010101010101010101010101010101010101010101")
                .unwrap();
        signer
            .sign_request(
                action.to_json().unwrap(),
                1,
                None,
                &SigningDomain::new(12589),
                ActionInfo {
                    kind: action.kind(),
                    expected_legs: action.expected_legs(),
                    targets: action.targets(),
                },
            )
            .unwrap()
    }

    fn place(n: usize) -> SignedRequest {
        request(&Place::new(
            (0..n)
                .map(|_| Order::limit(1, true, "1", "1", Tif::Alo))
                .collect(),
        ))
    }

    #[test]
    fn parses_every_leg_kind() {
        let body = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[
            {"resting":{"oid":11,"cloid":"0x00000000000000000000000000000001"}},
            {"filled":{"totalSz":"0.1","avgPx":"100.5","oid":12}},
            {"error":"Order must have minimum value of $10."}]}}}"#;
        let r = interpret(body, &place(3)).unwrap();
        assert_eq!(r.legs.len(), 3);
        assert_eq!(r.oids(), vec![Some(11), Some(12), None]);
        assert!(!r.all_ok());
        assert_eq!(
            r.rejected_legs(),
            vec![(2, "Order must have minimum value of $10.")]
        );
        assert!(
            matches!(&r.legs[1], LegStatus::Filled { total_sz, avg_px, .. } if total_sz == "0.1" && avg_px == "100.5")
        );
    }

    #[test]
    fn cancel_legs_and_waiting_strings() {
        let req = request(&Cancel::new(vec![
            CancelItem { asset: 1, oid: 1 },
            CancelItem { asset: 1, oid: 2 },
        ]));
        let body = br#"{"status":"ok","response":{"type":"cancel","data":{"statuses":["success",{"error":"Order was never placed, already canceled, or filled."}]}}}"#;
        let r = interpret(body, &req).unwrap();
        assert_eq!(r.legs[0], LegStatus::Success);
        assert!(r.legs[1].is_error());
        let w = LegStatus::from_value(&serde_json::json!("waitingForTrigger"));
        assert!(matches!(w, LegStatus::Waiting { .. }));
    }

    #[test]
    fn single_modify_default_ack() {
        let req = request(&Modify {
            oid: 5,
            order: Order::limit(1, true, "1", "1", Tif::Alo),
        });
        let r = interpret(br#"{"status":"ok","response":{"type":"default"}}"#, &req).unwrap();
        assert!(r.default_ack && r.legs.is_empty());
        // The same reply for an order action is not a receipt.
        let e = interpret(
            br#"{"status":"ok","response":{"type":"default"}}"#,
            &place(1),
        )
        .unwrap_err();
        assert!(e.is_unknown_outcome());
    }

    #[test]
    fn batch_modify_receipt_carries_new_oid() {
        let req = request(&BatchModify::new(vec![ModifyItem {
            oid: 5,
            order: Order::limit(1, true, "1", "1", Tif::Alo),
        }]));
        let body = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":99}}]}}}"#;
        assert_eq!(interpret(body, &req).unwrap().oids(), vec![Some(99)]);
    }

    #[test]
    fn explicit_rejections() {
        let body = br#"{"status":"err","response":"action rejected by execution (code 91): User or API Wallet does not exist"}"#;
        let e = interpret(body, &place(1)).unwrap_err();
        let r = e.as_rejected().unwrap();
        assert_eq!(r.code, Some(91));
        let e = interpret(
            br#"{"status":"err","response":"Attempted to modify to invalid new order"}"#,
            &place(1),
        )
        .unwrap_err();
        assert_eq!(e.as_rejected().unwrap().code, None);
        // PrecisionExceeded: one status for a two-leg action, no oid allocated.
        let body = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"PrecisionExceeded"}]}}}"#;
        let e = interpret(body, &place(2)).unwrap_err();
        assert!(e.as_rejected().is_some());
    }

    #[test]
    fn ambiguous_replies_are_unknown_outcomes() {
        let cases: [&[u8]; 7] = [
            br#"not json"#,
            br#"{"nostatus":1}"#,
            br#"{"status":"ok","response":{"type":"order","data":{"statuses":[]}}}"#,
            br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":1}},{"mystery":1}]}}}"#,
            br#"{"status":"err","response":"order not verified, query the order status before retrying"}"#,
            br#"{"status":"err","response":"action already pending"}"#,
            br#"{"status":"err","response":"x","committedResult":{"type":"order","data":{"statuses":[{"resting":{"oid":3}},{"error":"boom"}]}}}"#,
        ];
        for body in cases {
            let e = interpret(body, &place(2)).unwrap_err();
            assert!(e.is_unknown_outcome(), "{}", String::from_utf8_lossy(body));
        }
        // The partial legs are preserved for reconciliation.
        let e = interpret(cases[6], &place(2)).unwrap_err();
        assert_eq!(e.as_unknown_outcome().unwrap().legs.len(), 2);
    }

    #[test]
    fn committed_plain_errors_are_a_final_rejection() {
        let body = br#"{"status":"err","response":"x","committedResult":{"type":"order","data":{"statuses":[{"error":"bad"},{"error":"worse"}]}}}"#;
        let e = interpret(body, &place(2)).unwrap_err();
        assert_eq!(e.as_rejected().unwrap().legs.len(), 2);
    }

    #[test]
    fn duplicate_nonce_and_pending_are_never_rejections() {
        let cases: [&[u8]; 4] = [
            br#"{"status":"err","response":"action rejected by execution (code 205): duplicate nonce"}"#,
            br#"{"status":"err","response":"action rejected by execution (code 205)"}"#,
            br#"{"status":"err","response":"action already pending"}"#,
            br#"{"status":"err","response":"action rejected by execution (code 205): dup","committedResult":{"type":"order","data":{"statuses":[{"error":"bad"}]}}}"#,
        ];
        for body in cases {
            let req = place(1);
            let e = interpret(body, &req).unwrap_err();
            assert!(
                e.as_rejected().is_none(),
                "{}",
                String::from_utf8_lossy(body)
            );
            let u = e.as_unknown_outcome().expect("unknown outcome");
            assert_eq!(u.nonce(), req.nonce());
            assert_eq!(u.request.body(), req.body());
            assert_eq!(u.reason, UnknownReason::AmbiguousReceipt);
        }
        // Other execution codes stay explicit rejections.
        let body = br#"{"status":"err","response":"action rejected by execution (code 204): nonce too large"}"#;
        assert_eq!(
            interpret(body, &place(1))
                .unwrap_err()
                .as_rejected()
                .unwrap()
                .code,
            Some(204)
        );
    }

    #[test]
    fn only_precision_exceeded_rejects_a_short_receipt() {
        // Any other single error for a multi-leg action proves nothing.
        let body = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Order must have minimum value of $10."}]}}}"#;
        let e = interpret(body, &place(2)).unwrap_err();
        assert!(e.as_rejected().is_none());
        assert!(e.is_unknown_outcome());
        assert_eq!(e.as_unknown_outcome().unwrap().legs.len(), 1);
        // Too many statuses, or a short receipt of non-errors.
        for body in [
            &br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":1}},{"resting":{"oid":2}},{"resting":{"oid":3}}]}}}"#[..],
            &br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"resting":{"oid":1}}]}}}"#[..],
            &br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"PrecisionExceeded"},{"error":"PrecisionExceeded"},{"error":"x"}]}}}"#[..],
        ] {
            let e = interpret(body, &place(2)).unwrap_err();
            assert!(e.is_unknown_outcome(), "{}", String::from_utf8_lossy(body));
        }
        // PrecisionExceeded for a single-leg action is that leg's own result.
        let body = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"PrecisionExceeded"}]}}}"#;
        let r = interpret(body, &place(1)).unwrap();
        assert_eq!(r.rejected_legs(), vec![(0, "PrecisionExceeded")]);
        // Per-leg errors in a full-length receipt stay per-leg results.
        let body = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"bad"},{"resting":{"oid":5}}]}}}"#;
        let r = interpret(body, &place(2)).unwrap();
        assert_eq!(r.rejected_legs(), vec![(0, "bad")]);
        assert_eq!(r.oids(), vec![None, Some(5)]);
    }

    #[test]
    fn legs_without_a_valid_oid_are_unknown_outcomes() {
        for leg in [
            r#"{"resting":{}}"#,
            r#"{"resting":{"oid":"7"}}"#,
            r#"{"resting":{"oid":-1}}"#,
            r#"{"filled":{"totalSz":"1","avgPx":"2"}}"#,
            r#"{"filled":{"totalSz":"1","avgPx":"2","oid":null}}"#,
            r#"{"filled":{"oid":3}}"#,
        ] {
            let body = format!(
                r#"{{"status":"ok","response":{{"type":"order","data":{{"statuses":[{leg}]}}}}}}"#
            );
            let e = interpret(body.as_bytes(), &place(1)).unwrap_err();
            assert!(e.is_unknown_outcome(), "{leg}");
            assert_eq!(e.as_unknown_outcome().unwrap().legs.len(), 1);
        }
    }

    #[test]
    fn a_single_modify_with_statuses_needs_exactly_one() {
        let req = request(&Modify {
            oid: 5,
            order: Order::limit(1, true, "1", "1", Tif::Alo),
        });
        let two = br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"a"},{"error":"b"}]}}}"#;
        assert!(interpret(two, &req).unwrap_err().is_unknown_outcome());
        let one =
            br#"{"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"a"}]}}}"#;
        assert!(!interpret(one, &req).unwrap().all_ok());
    }

    #[test]
    fn execution_code_parsing() {
        assert_eq!(
            execution_code("action rejected by execution (code 1160): Guild requires Alo"),
            Some(1160)
        );
        assert_eq!(execution_code("something else"), None);
    }
}
