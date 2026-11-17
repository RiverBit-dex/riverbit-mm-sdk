//! Reconciliation after an unknown outcome or a reconnect.
//!
//! `unknownOid`, an empty open-order list, an elapsed timeout and a consumed
//! nonce do not prove that a request did not execute. The helpers here look
//! orders up by oid or cloid and report exactly what was found, so that the
//! caller can decide with evidence.

use crate::actions::OrderRef;
use crate::error::{Error, Result, UnknownOutcome};
use crate::info::{InfoClient, OrderState, OrderStatusReply};
use crate::signing::Address;

/// What one lookup found.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderReport {
    /// The oid or cloid that was looked up.
    pub target: OrderRef,
    /// The `orderStatus` answer.
    pub reply: OrderStatusReply,
}

impl OrderReport {
    /// True if the order was found in a known terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(&self.reply.state, OrderState::Order(o) if o.is_terminal())
    }

    /// True if the order was found and is open.
    pub fn is_open(&self) -> bool {
        matches!(&self.reply.state, OrderState::Order(o) if o.is_open())
    }

    /// True if the server does not know the id. That is **not** evidence that
    /// the request never executed.
    pub fn is_unknown_oid(&self) -> bool {
        matches!(self.reply.state, OrderState::UnknownOid)
    }
}

/// Looks up each target with `orderStatus` for the master account or vault `user`.
pub async fn reconcile_orders(
    info: &InfoClient,
    user: &Address,
    targets: &[OrderRef],
) -> Result<Vec<OrderReport>> {
    let mut reports = Vec::with_capacity(targets.len());
    for target in targets {
        let reply = info.order_status(user, *target).await?;
        reports.push(OrderReport {
            target: *target,
            reply,
        });
    }
    Ok(reports)
}

/// Reconciles an [`UnknownOutcome`] by looking up the oids and cloids its
/// request referred to.
///
/// `user` is the account the request traded for: the vault if the request
/// carried one, otherwise the master account. Orders that were never given a
/// cloid cannot be found this way; use `frontend_open_orders` and `user_fills`
/// for those.
pub async fn reconcile(
    info: &InfoClient,
    user: &Address,
    outcome: &UnknownOutcome,
) -> Result<Vec<OrderReport>> {
    let targets = outcome.request.targets();
    if targets.is_empty() {
        return Err(Error::InvalidInput(
            "the request has no oid or cloid to look up; compare frontendOpenOrders and userFills instead".into(),
        ));
    }
    reconcile_orders(info, user, targets).await
}
