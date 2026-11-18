//! Trading actions with fixed field order.
//!
//! The JSON these types serialize to is exactly what is signed and sent. Key
//! order is part of the signature: the node converts the received JSON to
//! MessagePack in received key order. Prices and sizes are decimal strings.

use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use crate::cloid::Cloid;
use crate::decimal::is_decimal_string;
use crate::error::Error;
use crate::signing::Address;

/// Time in force for a limit order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tif {
    /// Add liquidity only: an immediately marketable order is rejected.
    Alo,
    /// Good till cancelled.
    Gtc,
}

impl Tif {
    /// The wire string.
    pub fn as_str(self) -> &'static str {
        match self {
            Tif::Alo => "Alo",
            Tif::Gtc => "Gtc",
        }
    }
}

/// A reference to one order: by exchange order ID or by client order ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrderRef {
    /// Exchange-assigned order ID.
    Oid(u64),
    /// Client order ID.
    Cloid(Cloid),
}

/// The kind of a trading action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionKind {
    /// `order`
    Order,
    /// `cancel`
    Cancel,
    /// `cancelByCloid`
    CancelByCloid,
    /// `modify`
    Modify,
    /// `batchModify`
    BatchModify,
    /// `reserveRequestWeight`: buy request quota. It has no legs and refers
    /// to no order.
    ReserveRequestWeight,
}

impl ActionKind {
    /// The `type` string of the action.
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Order => "order",
            ActionKind::Cancel => "cancel",
            ActionKind::CancelByCloid => "cancelByCloid",
            ActionKind::Modify => "modify",
            ActionKind::BatchModify => "batchModify",
            ActionKind::ReserveRequestWeight => "reserveRequestWeight",
        }
    }
}

/// A signable trading action.
pub trait Action {
    /// The action kind.
    fn kind(&self) -> ActionKind;

    /// The action JSON, compact, with the fixed key order that is signed.
    /// Validates field formats first.
    fn to_json(&self) -> Result<String, Error>;

    /// How many entries `statuses[]` should have in the receipt, if the action
    /// is expected to return one per leg.
    fn expected_legs(&self) -> Option<usize>;

    /// The orders this action refers to, for reconciliation after an unknown
    /// outcome.
    fn targets(&self) -> Vec<OrderRef>;

    /// Checks the action against the account it will be sent for: `vault` is
    /// the vault of the client that signs it, if any. The default accepts
    /// everything. [`ExchangeClient::prepare`](crate::ExchangeClient::prepare)
    /// calls it before signing.
    fn check_account(&self, _vault: Option<&Address>) -> Result<(), Error> {
        Ok(())
    }
}

/// One limit order leg: `{"a","b","p","s","r","t":{"limit":{"tif"}},"c"?}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    /// Asset index: the position in `universe` from `metaAndAssetCtxs`.
    pub asset: u32,
    /// True to buy.
    pub is_buy: bool,
    /// Limit price as a decimal string.
    pub price: String,
    /// Size as a decimal string.
    pub size: String,
    /// Reduce-only flag.
    pub reduce_only: bool,
    /// Time in force.
    pub tif: Tif,
    /// Optional client order ID.
    pub cloid: Option<Cloid>,
}

impl Order {
    /// A limit order, not reduce-only and without a cloid.
    pub fn limit(
        asset: u32,
        is_buy: bool,
        price: impl Into<String>,
        size: impl Into<String>,
        tif: Tif,
    ) -> Self {
        Order {
            asset,
            is_buy,
            price: price.into(),
            size: size.into(),
            reduce_only: false,
            tif,
            cloid: None,
        }
    }

    /// Sets the reduce-only flag.
    pub fn reduce_only(mut self, reduce_only: bool) -> Self {
        self.reduce_only = reduce_only;
        self
    }

    /// Sets the client order ID.
    pub fn with_cloid(mut self, cloid: Cloid) -> Self {
        self.cloid = Some(cloid);
        self
    }

    /// Checks that price and size are plain decimal strings.
    pub fn validate(&self) -> Result<(), Error> {
        check_decimal("price", &self.price)?;
        check_decimal("size", &self.size)
    }
}

fn check_decimal(name: &str, value: &str) -> Result<(), Error> {
    if is_decimal_string(value) {
        Ok(())
    } else {
        Err(Error::InvalidInput(format!(
            "{name} {value:?} is not a plain decimal string (digits with an optional fraction)"
        )))
    }
}

struct Limit(Tif);
impl Serialize for Limit {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Limit", 1)?;
        st.serialize_field("tif", self.0.as_str())?;
        st.end()
    }
}
struct OrderType(Tif);
impl Serialize for OrderType {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("OrderType", 1)?;
        st.serialize_field("limit", &Limit(self.0))?;
        st.end()
    }
}

impl Serialize for Order {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let len = 6 + usize::from(self.cloid.is_some());
        let mut st = s.serialize_struct("Order", len)?;
        st.serialize_field("a", &self.asset)?;
        st.serialize_field("b", &self.is_buy)?;
        st.serialize_field("p", &self.price)?;
        st.serialize_field("s", &self.size)?;
        st.serialize_field("r", &self.reduce_only)?;
        st.serialize_field("t", &OrderType(self.tif))?;
        if let Some(c) = &self.cloid {
            st.serialize_field("c", c)?;
        }
        st.end()
    }
}

/// The `order` action: place one or more limit orders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// The order legs, in order.
    pub orders: Vec<Order>,
}

impl Place {
    /// An `order` action with `grouping` `na`.
    pub fn new(orders: Vec<Order>) -> Self {
        Place { orders }
    }
}

impl Serialize for Place {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Place", 3)?;
        st.serialize_field("type", "order")?;
        st.serialize_field("orders", &self.orders)?;
        st.serialize_field("grouping", "na")?;
        st.end()
    }
}

/// One entry of a `cancel` action: `{"a": asset, "o": oid}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelItem {
    /// Asset index.
    pub asset: u32,
    /// Exchange order ID.
    pub oid: u64,
}

impl Serialize for CancelItem {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("CancelItem", 2)?;
        st.serialize_field("a", &self.asset)?;
        st.serialize_field("o", &self.oid)?;
        st.end()
    }
}

/// The `cancel` action: cancel orders by exchange order ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancel {
    /// The cancellations, in order.
    pub cancels: Vec<CancelItem>,
}

impl Cancel {
    /// A `cancel` action.
    pub fn new(cancels: Vec<CancelItem>) -> Self {
        Cancel { cancels }
    }
}

impl Serialize for Cancel {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Cancel", 2)?;
        st.serialize_field("type", "cancel")?;
        st.serialize_field("cancels", &self.cancels)?;
        st.end()
    }
}

/// One entry of a `cancelByCloid` action: `{"asset": asset, "cloid": cloid}`.
/// Note the field names differ from `cancel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelByCloidItem {
    /// Asset index.
    pub asset: u32,
    /// Client order ID.
    pub cloid: Cloid,
}

impl Serialize for CancelByCloidItem {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("CancelByCloidItem", 2)?;
        st.serialize_field("asset", &self.asset)?;
        st.serialize_field("cloid", &self.cloid)?;
        st.end()
    }
}

/// The `cancelByCloid` action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelByCloid {
    /// The cancellations, in order.
    pub cancels: Vec<CancelByCloidItem>,
}

impl CancelByCloid {
    /// A `cancelByCloid` action.
    pub fn new(cancels: Vec<CancelByCloidItem>) -> Self {
        CancelByCloid { cancels }
    }
}

impl Serialize for CancelByCloid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("CancelByCloid", 2)?;
        st.serialize_field("type", "cancelByCloid")?;
        st.serialize_field("cancels", &self.cancels)?;
        st.end()
    }
}

/// The `modify` action: replace one order with a complete new order.
///
/// An accepted modify returns `{"type":"default"}` without the new oid; prefer
/// [`BatchModify`], whose receipt carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Modify {
    /// The order to replace.
    pub oid: u64,
    /// The complete replacement order.
    pub order: Order,
}

impl Serialize for Modify {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Modify", 3)?;
        st.serialize_field("type", "modify")?;
        st.serialize_field("oid", &self.oid)?;
        st.serialize_field("order", &self.order)?;
        st.end()
    }
}

/// One entry of a `batchModify` action: `{"oid": oid, "order": {...}}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModifyItem {
    /// The order to replace.
    pub oid: u64,
    /// The complete replacement order. Supply the original cloid to keep it.
    pub order: Order,
}

impl Serialize for ModifyItem {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("ModifyItem", 2)?;
        st.serialize_field("oid", &self.oid)?;
        st.serialize_field("order", &self.order)?;
        st.end()
    }
}

/// The `batchModify` action. `statuses[]` corresponds to `modifies[]` and a
/// successful `resting.oid` is the new oid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchModify {
    /// The modifications, in order.
    pub modifies: Vec<ModifyItem>,
}

impl BatchModify {
    /// A `batchModify` action.
    pub fn new(modifies: Vec<ModifyItem>) -> Self {
        BatchModify { modifies }
    }
}

impl Serialize for BatchModify {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("BatchModify", 2)?;
        st.serialize_field("type", "batchModify")?;
        st.serialize_field("modifies", &self.modifies)?;
        st.end()
    }
}

/// The smallest `weight` of a [`ReserveRequestWeight`] purchase.
pub const MIN_RESERVE_WEIGHT: u64 = 1;
/// The largest `weight` of a single [`ReserveRequestWeight`] purchase.
pub const MAX_RESERVE_WEIGHT: u64 = 1_000_000_000;
/// The price of one request, in micro-USDC (0.0005 USDC).
pub const REQUEST_PRICE_MICRO_USDC: u64 = 500;

/// The `reserveRequestWeight` action: buy request quota.
///
/// Each account has a cumulative cap of ordinary requests (see
/// [`UserRateLimit`](crate::info::UserRateLimit)); purchased requests add to
/// it. A request costs 0.0005 USDC, paid by the payer. The purchase itself
/// does not consume quota.
///
/// Without a vault on the client the payer is the signer's account (for an API
/// wallet, its master account) and `destination` names the account that
/// receives the requests; absent, the payer receives them. To have the master
/// pay for a vault, send it from a client **without** `for_vault` and set
/// `destination` to the vault. With a vault client the vault pays and
/// receives, so `destination` must be absent or equal to the vault (vault-paid
/// purchases may be disabled on a network).
///
/// Signing and submission are the same as for `order`. The receipt has no
/// legs: success is `Receipt::default_ack`. An unknown outcome of a purchase
/// has no order to look up; compare
/// [`InfoClient::user_rate_limit`](crate::InfoClient::user_rate_limit) before
/// and after (`nRequestsSurplus` and `nRequestsUsed`), and never sign it again
/// with a new nonce. The SDK never buys on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReserveRequestWeight {
    /// Number of requests to buy, `1..=1_000_000_000`.
    pub weight: u64,
    /// The account that receives the requests. `None` means the payer.
    pub destination: Option<Address>,
}

impl ReserveRequestWeight {
    /// Buys `weight` requests for the payer. `weight` is checked when the
    /// action is serialized.
    pub fn new(weight: u64) -> Self {
        ReserveRequestWeight {
            weight,
            destination: None,
        }
    }

    /// Sends the requests to `destination` instead of the payer.
    pub fn with_destination(mut self, destination: Address) -> Self {
        self.destination = Some(destination);
        self
    }

    /// The cost in micro-USDC: `weight * 500`, saturating.
    pub fn cost_micro_usdc(&self) -> u64 {
        cost_micro_usdc(self.weight)
    }

    /// Checks `1 <= weight <= 1_000_000_000`.
    pub fn validate(&self) -> Result<(), Error> {
        if (MIN_RESERVE_WEIGHT..=MAX_RESERVE_WEIGHT).contains(&self.weight) {
            Ok(())
        } else {
            Err(Error::InvalidInput(format!(
                "weight {} is outside {MIN_RESERVE_WEIGHT}..={MAX_RESERVE_WEIGHT}",
                self.weight
            )))
        }
    }
}

/// The cost of `weight` requests in micro-USDC (`weight * 500`, saturating).
pub fn cost_micro_usdc(weight: u64) -> u64 {
    weight.saturating_mul(REQUEST_PRICE_MICRO_USDC)
}

impl Serialize for ReserveRequestWeight {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let len = 2 + usize::from(self.destination.is_some());
        let mut st = s.serialize_struct("ReserveRequestWeight", len)?;
        st.serialize_field("type", "reserveRequestWeight")?;
        st.serialize_field("weight", &self.weight)?;
        if let Some(d) = &self.destination {
            st.serialize_field("destination", &d.to_string())?;
        }
        st.end()
    }
}

fn non_empty<T>(name: &str, items: &[T]) -> Result<(), Error> {
    if items.is_empty() {
        Err(Error::InvalidInput(format!("{name} must not be empty")))
    } else {
        Ok(())
    }
}

fn to_json<T: Serialize>(action: &T) -> Result<String, Error> {
    serde_json::to_string(action).map_err(|e| Error::InvalidInput(format!("serialize action: {e}")))
}

impl Action for Place {
    fn kind(&self) -> ActionKind {
        ActionKind::Order
    }
    fn to_json(&self) -> Result<String, Error> {
        non_empty("orders", &self.orders)?;
        self.orders.iter().try_for_each(Order::validate)?;
        to_json(self)
    }
    fn expected_legs(&self) -> Option<usize> {
        Some(self.orders.len())
    }
    fn targets(&self) -> Vec<OrderRef> {
        self.orders
            .iter()
            .filter_map(|o| o.cloid.map(OrderRef::Cloid))
            .collect()
    }
}

impl Action for Cancel {
    fn kind(&self) -> ActionKind {
        ActionKind::Cancel
    }
    fn to_json(&self) -> Result<String, Error> {
        non_empty("cancels", &self.cancels)?;
        to_json(self)
    }
    fn expected_legs(&self) -> Option<usize> {
        Some(self.cancels.len())
    }
    fn targets(&self) -> Vec<OrderRef> {
        self.cancels.iter().map(|c| OrderRef::Oid(c.oid)).collect()
    }
}

impl Action for CancelByCloid {
    fn kind(&self) -> ActionKind {
        ActionKind::CancelByCloid
    }
    fn to_json(&self) -> Result<String, Error> {
        non_empty("cancels", &self.cancels)?;
        to_json(self)
    }
    fn expected_legs(&self) -> Option<usize> {
        Some(self.cancels.len())
    }
    fn targets(&self) -> Vec<OrderRef> {
        self.cancels
            .iter()
            .map(|c| OrderRef::Cloid(c.cloid))
            .collect()
    }
}

impl Action for Modify {
    fn kind(&self) -> ActionKind {
        ActionKind::Modify
    }
    fn to_json(&self) -> Result<String, Error> {
        self.order.validate()?;
        to_json(self)
    }
    fn expected_legs(&self) -> Option<usize> {
        // An accepted modify returns {"type":"default"}; a rejection carries statuses.
        None
    }
    fn targets(&self) -> Vec<OrderRef> {
        let mut t = vec![OrderRef::Oid(self.oid)];
        t.extend(self.order.cloid.map(OrderRef::Cloid));
        t
    }
}

impl Action for BatchModify {
    fn kind(&self) -> ActionKind {
        ActionKind::BatchModify
    }
    fn to_json(&self) -> Result<String, Error> {
        non_empty("modifies", &self.modifies)?;
        self.modifies.iter().try_for_each(|m| m.order.validate())?;
        to_json(self)
    }
    fn expected_legs(&self) -> Option<usize> {
        Some(self.modifies.len())
    }
    fn targets(&self) -> Vec<OrderRef> {
        let mut t = Vec::new();
        for m in &self.modifies {
            t.push(OrderRef::Oid(m.oid));
            t.extend(m.order.cloid.map(OrderRef::Cloid));
        }
        t
    }
}

impl Action for ReserveRequestWeight {
    fn kind(&self) -> ActionKind {
        ActionKind::ReserveRequestWeight
    }
    fn to_json(&self) -> Result<String, Error> {
        self.validate()?;
        to_json(self)
    }
    fn expected_legs(&self) -> Option<usize> {
        // A purchase has no legs: success is `{"type":"default"}`.
        None
    }
    fn targets(&self) -> Vec<OrderRef> {
        // No order to look up; see the type documentation.
        vec![]
    }
    fn check_account(&self, vault: Option<&Address>) -> Result<(), Error> {
        match (vault, &self.destination) {
            (Some(v), Some(d)) if v != d => Err(Error::InvalidInput(
                "with a vault client the vault pays and receives: destination must be absent or equal to the vault; \
                 to have the master pay for a vault, use a client without a vault and set destination"
                    .into(),
            )),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_json_matches_guide_example() {
        let place = Place::new(vec![Order::limit(0, true, "100.5", "0.2", Tif::Alo)]);
        assert_eq!(
            place.to_json().unwrap(),
            r#"{"type":"order","orders":[{"a":0,"b":true,"p":"100.5","s":"0.2","r":false,"t":{"limit":{"tif":"Alo"}}}],"grouping":"na"}"#
        );
    }

    #[test]
    fn cloid_is_the_last_order_key() {
        let order =
            Order::limit(1, false, "82000", "0.001", Tif::Gtc).with_cloid(Cloid::from_u128(2));
        assert_eq!(
            serde_json::to_string(&order).unwrap(),
            r#"{"a":1,"b":false,"p":"82000","s":"0.001","r":false,"t":{"limit":{"tif":"Gtc"}},"c":"0x00000000000000000000000000000002"}"#
        );
    }

    #[test]
    fn cancel_actions_use_different_field_names() {
        let cancel = Cancel::new(vec![CancelItem {
            asset: 1,
            oid: 123456789,
        }]);
        assert_eq!(
            cancel.to_json().unwrap(),
            r#"{"type":"cancel","cancels":[{"a":1,"o":123456789}]}"#
        );
        let by_cloid = CancelByCloid::new(vec![CancelByCloidItem {
            asset: 1,
            cloid: Cloid::from_u128(1),
        }]);
        assert_eq!(
            by_cloid.to_json().unwrap(),
            r#"{"type":"cancelByCloid","cancels":[{"asset":1,"cloid":"0x00000000000000000000000000000001"}]}"#
        );
    }

    #[test]
    fn modify_actions_match_guide() {
        let order =
            Order::limit(1, true, "80100", "0.001", Tif::Alo).with_cloid(Cloid::from_u128(1));
        let batch = BatchModify::new(vec![ModifyItem {
            oid: 123456789,
            order: order.clone(),
        }]);
        assert_eq!(
            batch.to_json().unwrap(),
            r#"{"type":"batchModify","modifies":[{"oid":123456789,"order":{"a":1,"b":true,"p":"80100","s":"0.001","r":false,"t":{"limit":{"tif":"Alo"}},"c":"0x00000000000000000000000000000001"}}]}"#
        );
        let single = Modify { oid: 7, order };
        assert!(single
            .to_json()
            .unwrap()
            .starts_with(r#"{"type":"modify","oid":7,"order":{"a":1"#));
    }

    #[test]
    fn rejects_non_decimal_price_and_size() {
        for (p, s) in [
            (".5", "1"),
            ("1", "-1"),
            ("1e3", "1"),
            ("1", "+1"),
            ("1 ", "1"),
            ("", "1"),
            ("1", ""),
        ] {
            let place = Place::new(vec![Order::limit(0, true, p, s, Tif::Alo)]);
            assert!(place.to_json().is_err(), "{p:?} {s:?}");
        }
        assert!(Place::new(vec![]).to_json().is_err());
    }

    #[test]
    fn targets_list_oids_and_cloids() {
        let c = Cloid::from_u128(9);
        let order = Order::limit(1, true, "1", "1", Tif::Alo).with_cloid(c);
        assert_eq!(
            Place::new(vec![order.clone()]).targets(),
            vec![OrderRef::Cloid(c)]
        );
        assert_eq!(
            BatchModify::new(vec![ModifyItem { oid: 5, order }]).targets(),
            vec![OrderRef::Oid(5), OrderRef::Cloid(c)]
        );
    }

    #[test]
    fn reserve_request_weight_json_and_validation() {
        assert_eq!(
            ReserveRequestWeight::new(1000).to_json().unwrap(),
            r#"{"type":"reserveRequestWeight","weight":1000}"#
        );
        let dest: Address = "0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap();
        assert_eq!(
            ReserveRequestWeight::new(1000)
                .with_destination(dest)
                .to_json()
                .unwrap(),
            r#"{"type":"reserveRequestWeight","weight":1000,"destination":"0x2222222222222222222222222222222222222222"}"#
        );
        for bad in [0, MAX_RESERVE_WEIGHT + 1, u64::MAX] {
            assert!(ReserveRequestWeight::new(bad).to_json().is_err(), "{bad}");
        }
        for ok in [MIN_RESERVE_WEIGHT, MAX_RESERVE_WEIGHT] {
            assert!(ReserveRequestWeight::new(ok).to_json().is_ok());
        }
        assert_eq!(ReserveRequestWeight::new(1000).cost_micro_usdc(), 500_000);
        assert_eq!(cost_micro_usdc(u64::MAX), u64::MAX);
        assert!(ReserveRequestWeight::new(1).targets().is_empty());
        assert_eq!(ReserveRequestWeight::new(1).expected_legs(), None);
    }

    #[test]
    fn reserve_request_weight_destination_must_match_a_vault() {
        let vault: Address = "0x1111111111111111111111111111111111111111"
            .parse()
            .unwrap();
        let other: Address = "0x2222222222222222222222222222222222222222"
            .parse()
            .unwrap();
        let plain = ReserveRequestWeight::new(5);
        assert!(plain.check_account(None).is_ok());
        assert!(plain.check_account(Some(&vault)).is_ok());
        let to_vault = ReserveRequestWeight::new(5).with_destination(vault);
        assert!(to_vault.check_account(None).is_ok());
        assert!(to_vault.check_account(Some(&vault)).is_ok());
        let to_other = ReserveRequestWeight::new(5).with_destination(other);
        assert!(to_other.check_account(None).is_ok());
        assert!(to_other.check_account(Some(&vault)).is_err());
    }
}
