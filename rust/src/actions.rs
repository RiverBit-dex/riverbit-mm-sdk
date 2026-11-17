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
}
