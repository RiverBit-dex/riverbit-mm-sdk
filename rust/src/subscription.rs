//! Subscription objects for `subscribe_json` and `unsubscribe_json`.

use serde_json::{json, Value};

use crate::signing::Address;

/// A bin-ws subscription: the JSON object itself, without a `method` wrapper.
///
/// Unsubscribe with an equal value (same constructor, same arguments).
#[derive(Clone, Debug, PartialEq)]
pub struct Subscription(Value);

impl Subscription {
    /// Incremental book: `{"type":"l2","c":coin}`. A snapshot, then updates;
    /// replay them with [`Book`](crate::Book).
    pub fn l2(coin: &str) -> Self {
        Subscription(json!({"type": "l2", "c": coin}))
    }

    /// Incremental book bucketed to `sig_figs` significant figures (`s`).
    pub fn l2_sig_figs(coin: &str, sig_figs: u8) -> Self {
        Subscription(json!({"type": "l2", "c": coin, "s": sig_figs}))
    }

    /// Full book: `{"type":"l2Book","coin":coin}`.
    pub fn l2_book(coin: &str) -> Self {
        Subscription(json!({"type": "l2Book", "coin": coin}))
    }

    /// Best bid and offer.
    pub fn bbo(coin: &str) -> Self {
        Subscription(json!({"type": "bbo", "coin": coin}))
    }

    /// Trades.
    pub fn trades(coin: &str) -> Self {
        Subscription(json!({"type": "trades", "coin": coin}))
    }

    /// Candles; intervals are `1m 3m 5m 15m 30m 1h 2h 4h 8h 12h 1d 3d 1w`.
    pub fn candle(coin: &str, interval: &str) -> Self {
        Subscription(json!({"type": "candle", "coin": coin, "interval": interval}))
    }

    /// Order updates of a master account or vault. Only changes after the
    /// subscription are sent; read `openOrders` separately for the base state.
    pub fn order_updates(user: &Address) -> Self {
        Self::for_user("orderUpdates", user)
    }

    /// Open orders of a master account or vault.
    pub fn open_orders(user: &Address) -> Self {
        Self::for_user("openOrders", user)
    }

    /// Fills of a master account or vault.
    pub fn user_fills(user: &Address) -> Self {
        Self::for_user("userFills", user)
    }

    /// Positions and margin of a master account or vault.
    pub fn clearinghouse_state(user: &Address) -> Self {
        Self::for_user("clearinghouseState", user)
    }

    /// Any other subscription object.
    pub fn raw(value: Value) -> Self {
        Subscription(value)
    }

    fn for_user(kind: &str, user: &Address) -> Self {
        Subscription(json!({"type": kind, "user": user.to_string()}))
    }

    /// The JSON value.
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.0).expect("a JSON value always serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_uses_c_not_coin() {
        assert_eq!(
            Subscription::l2("BTC").to_bytes(),
            br#"{"type":"l2","c":"BTC"}"#
        );
        assert_eq!(
            Subscription::l2_sig_figs("BTC", 5).to_bytes(),
            br#"{"type":"l2","c":"BTC","s":5}"#
        );
        assert_eq!(
            Subscription::l2_book("BTC").to_bytes(),
            br#"{"type":"l2Book","coin":"BTC"}"#
        );
        assert_eq!(
            Subscription::candle("BTC", "1m").to_bytes(),
            br#"{"type":"candle","coin":"BTC","interval":"1m"}"#
        );
    }
}
