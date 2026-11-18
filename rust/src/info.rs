//! The `/info` client and its typed replies.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::actions::OrderRef;
use crate::error::{Error, Result};
use crate::signing::Address;
use crate::transport::{InfoTransport, Rest, RestInfo, TransportError};

/// `chainIdentity`: the values needed for signing.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ChainIdentity {
    /// The EIP-712 domain chain ID for trading signatures. Always read it from
    /// here; never hard-code it.
    #[serde(rename = "nextAgentChainId")]
    pub next_agent_chain_id: u64,
}

/// `userRole`: what an address is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserRole {
    /// An API wallet; `user` is its master account.
    Agent {
        /// The master account the wallet acts for.
        user: String,
    },
    /// A master account.
    User,
    /// A sub-account; `master` owns it.
    SubAccount {
        /// The owning master account.
        master: String,
    },
    /// A vault.
    Vault,
    /// Unknown to the exchange.
    Missing,
    /// A role this crate does not know; the raw reply.
    Other(Value),
}

impl UserRole {
    fn from_value(v: Value) -> UserRole {
        let data_str = |key: &str| v["data"][key].as_str().map(str::to_string);
        match v["role"].as_str() {
            Some("agent") => match data_str("user") {
                Some(user) => UserRole::Agent { user },
                None => UserRole::Other(v),
            },
            Some("user") => UserRole::User,
            Some("subAccount") => match data_str("master") {
                Some(master) => UserRole::SubAccount { master },
                None => UserRole::Other(v),
            },
            Some("vault") => UserRole::Vault,
            Some("missing") => UserRole::Missing,
            _ => UserRole::Other(v),
        }
    }
}

/// One asset in `universe`. Its position in the list is the order field `a`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct AssetMeta {
    /// Coin name.
    pub name: String,
    /// Size decimals: orders may not use more.
    #[serde(rename = "szDecimals")]
    pub sz_decimals: u32,
    /// Maximum leverage, if reported.
    #[serde(rename = "maxLeverage", default)]
    pub max_leverage: Option<u32>,
}

/// The `meta` half of `metaAndAssetCtxs`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Meta {
    /// All assets. The index of an asset in this list is its asset index.
    pub universe: Vec<AssetMeta>,
}

impl Meta {
    /// The asset index of `coin`, if listed.
    pub fn asset_index(&self, coin: &str) -> Option<u32> {
        self.universe
            .iter()
            .position(|a| a.name == coin)
            .map(|i| i as u32)
    }

    /// The asset with this name, if listed.
    pub fn asset(&self, coin: &str) -> Option<&AssetMeta> {
        self.universe.iter().find(|a| a.name == coin)
    }
}

/// `metaAndAssetCtxs`.
#[derive(Clone, Debug, PartialEq)]
pub struct MetaAndAssetCtxs {
    /// Asset metadata.
    pub meta: Meta,
    /// Per-asset market contexts, same order as `meta.universe`, as raw JSON.
    pub asset_ctxs: Vec<Value>,
}

/// One price level of an `l2Book` reply.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BookLevel {
    /// Price, a decimal string.
    pub px: String,
    /// Size, a decimal string.
    pub sz: String,
    /// Number of orders at the level.
    #[serde(default)]
    pub n: u32,
}

/// `l2Book`: `levels[0]` are bids, `levels[1]` are asks.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct L2Book {
    /// Coin name.
    pub coin: String,
    /// Milliseconds since the epoch.
    pub time: u64,
    /// Bids then asks, best first.
    pub levels: [Vec<BookLevel>; 2],
}

/// One entry of `openOrders`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct OpenOrder {
    /// Coin name.
    pub coin: String,
    /// `B` buy or `A` sell.
    pub side: String,
    /// Limit price, a decimal string.
    #[serde(rename = "limitPx")]
    pub limit_px: String,
    /// Remaining size, a decimal string.
    pub sz: String,
    /// Exchange order ID.
    pub oid: u64,
    /// Milliseconds since the epoch.
    #[serde(default)]
    pub timestamp: u64,
}

/// One entry of `frontendOpenOrders`: `openOrders` plus cloid, original size
/// and time in force.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct FrontendOpenOrder {
    /// Coin name.
    pub coin: String,
    /// `B` buy or `A` sell.
    pub side: String,
    /// Limit price, a decimal string.
    #[serde(rename = "limitPx")]
    pub limit_px: String,
    /// Remaining size, a decimal string.
    pub sz: String,
    /// Exchange order ID.
    pub oid: u64,
    /// Milliseconds since the epoch.
    #[serde(default)]
    pub timestamp: u64,
    /// Client order ID, if the order has one.
    #[serde(default)]
    pub cloid: Option<String>,
    /// Original size, a decimal string.
    #[serde(rename = "origSz", default)]
    pub orig_sz: Option<String>,
    /// Time in force.
    #[serde(default)]
    pub tif: Option<String>,
}

/// Order statuses that mean the order is no longer live.
const INACTIVE: [&str; 16] = [
    "filled",
    "canceled",
    "rejected",
    "tickRejected",
    "minTradeNtlRejected",
    "reduceOnlyRejected",
    "badAloPxRejected",
    "iocCancelRejected",
    "perpMarginRejected",
    "oracleRejected",
    "insufficientSpotBalanceRejected",
    "selfTradeCanceled",
    "reduceOnlyCanceled",
    "siblingFilledCanceled",
    "scheduledCancel",
    "vaultWithdrawalCanceled",
];

/// The current state of one order, from `orderStatus` with `status: "order"`.
#[derive(Clone, Debug, PartialEq)]
pub struct CurrentOrder {
    /// The order status, for example `open`, `filled`, `canceled`.
    pub status: String,
    /// The order ID, if the reply carries it.
    pub oid: Option<u64>,
    /// The client order ID, if any.
    pub cloid: Option<String>,
    /// The full `order.order` object as sent by the server.
    pub order: Value,
}

impl CurrentOrder {
    /// True for a known terminal status (filled, canceled or any rejection).
    /// An unknown status is neither terminal nor open: do not treat it as final.
    pub fn is_terminal(&self) -> bool {
        INACTIVE.contains(&self.status.as_str())
    }

    /// True if the order is open.
    pub fn is_open(&self) -> bool {
        self.status == "open"
    }
}

/// What `orderStatus` knows about a target.
#[derive(Clone, Debug, PartialEq)]
pub enum OrderState {
    /// The current order, with its status.
    Order(CurrentOrder),
    /// `legResult`: the latest operation result for the oid. It is **not** the
    /// current order state: a canceled order can still show a `resting` leg.
    LegResult(Value),
    /// `unknownOid`. This is **not** proof that the request never executed:
    /// the answering node may trail the validator by a couple of blocks.
    UnknownOid,
}

/// The reply to an `orderStatus` query.
#[derive(Clone, Debug, PartialEq)]
pub struct OrderStatusReply {
    /// The query height of the answering node, if reported. It is not the
    /// execution height of the original transaction.
    pub height: Option<u64>,
    /// What was found.
    pub state: OrderState,
}

impl OrderStatusReply {
    fn from_value(v: Value) -> Result<Self> {
        let height = v["height"].as_u64();
        let state = match v["status"].as_str() {
            Some("unknownOid") => OrderState::UnknownOid,
            Some("legResult") => OrderState::LegResult(v["leg"].clone()),
            Some("order") => {
                let status = v["order"]["status"]
                    .as_str()
                    .ok_or_else(|| Error::Decode("orderStatus: order without status".into()))?
                    .to_string();
                let inner = v["order"]["order"].clone();
                OrderState::Order(CurrentOrder {
                    status,
                    oid: inner["oid"].as_u64(),
                    cloid: inner["cloid"].as_str().map(str::to_string),
                    order: inner,
                })
            }
            other => {
                return Err(Error::Decode(format!(
                    "orderStatus: unexpected status {other:?}"
                )));
            }
        };
        Ok(OrderStatusReply { height, state })
    }
}

/// The `req` of a `candleSnapshot` query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandleRequest {
    /// Coin name.
    pub coin: String,
    /// Interval such as `1m`.
    pub interval: String,
    /// Start time in milliseconds.
    pub start_time: u64,
    /// End time in milliseconds.
    pub end_time: u64,
}

/// `userRateLimit`: the request quota of a master account or vault.
///
/// The cap is `10000 + floor(cumVlm)` ordinary requests; purchased requests
/// are not part of it. At most one of `n_requests_used` and
/// `n_requests_surplus` is above zero: `used` is `max(0, used - purchased)` and
/// `surplus` is `max(0, purchased - used)`. A node that cannot answer replies
/// with HTTP 501, which is reported as [`Error::Http`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRateLimit {
    /// Cumulative traded volume in USD, a decimal string (kept as a string).
    pub cum_vlm: String,
    /// The cap for ordinary actions, purchased requests not included.
    pub n_requests_cap: u64,
    /// `max(0, used - purchased)`.
    pub n_requests_used: u64,
    /// `max(0, purchased - used)`.
    pub n_requests_surplus: u64,
}

impl UserRateLimit {
    /// Requests left before the account is rejected:
    /// `max(0, cap - used) + surplus`, with saturating arithmetic.
    pub fn remaining(&self) -> u64 {
        self.n_requests_cap
            .saturating_sub(self.n_requests_used)
            .saturating_add(self.n_requests_surplus)
    }

    fn from_value(v: &Value) -> Result<Self> {
        let count = |name: &str| -> Result<u64> {
            match &v[name] {
                Value::Number(n) => n.as_u64(),
                Value::String(s) => s.trim().parse::<u64>().ok(),
                _ => None,
            }
            .ok_or_else(|| {
                Error::Decode(format!("userRateLimit: {name} is not an unsigned integer"))
            })
        };
        let cum_vlm = match &v["cumVlm"] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return Err(Error::Decode("userRateLimit: cumVlm is missing".into())),
        };
        Ok(UserRateLimit {
            cum_vlm,
            n_requests_cap: count("nRequestsCap")?,
            n_requests_used: count("nRequestsUsed")?,
            n_requests_surplus: count("nRequestsSurplus")?,
        })
    }
}

/// Query types that must use REST, never bin-ws `info_json`.
const REST_ONLY: [&str; 3] = ["metaAndAssetCtxs", "userRole", "leadingVaults"];

/// A client for `POST /info`.
///
/// Reads may be retried; a 429 is retried after its `Retry-After`. By default
/// everything goes over REST. [`with_bin_ws`](Self::with_bin_ws) routes the
/// queries bin-ws accepts over `info_json`; those answers come from a
/// read-only node that can trail REST by up to two blocks.
#[derive(Clone)]
pub struct InfoClient {
    rest_config: Rest,
    rest: Arc<dyn InfoTransport>,
    queries: Option<Arc<dyn InfoTransport>>,
    max_retries: u32,
    max_retry_wait: Duration,
}

impl InfoClient {
    /// A REST client for `base_url`, for example `https://pre-api.riverbit.io`.
    pub fn new(base_url: &str) -> Result<Self> {
        Self::with_http_client(base_url, None)
    }

    /// Like [`new`](Self::new) with your own `reqwest` client (proxies, timeouts).
    pub fn with_http_client(base_url: &str, http: Option<reqwest::Client>) -> Result<Self> {
        let rest = Rest::new(base_url, http)?;
        Ok(InfoClient {
            rest: Arc::new(RestInfo {
                rest: rest.clone(),
                timeout: Duration::from_secs(15),
            }),
            rest_config: rest,
            queries: None,
            max_retries: 3,
            max_retry_wait: Duration::from_secs(60),
        })
    }

    /// Sets the optional gateway credential. Every REST request of this
    /// client then carries it in the `X-RiverBit-MM-Token` header. An empty
    /// string clears it. It is never sent over bin-ws (queries routed with
    /// [`with_bin_ws`](Self::with_bin_ws) do not carry it), never logged and
    /// never part of `Debug` or error output. Characters that are not valid in
    /// a header value (CR, LF, spaces, non-ASCII) are rejected here.
    ///
    /// If you supply your own `reqwest` client, it must not follow redirects,
    /// or the credential would go to the redirect target. The default client
    /// does not. The credential only lifts gateway rate limits: it changes
    /// nothing about signing, and a wrong one is treated as none.
    pub fn with_gateway_token(mut self, token: &str) -> Result<Self> {
        self.rest_config.set_token(token)?;
        self.rest = Arc::new(RestInfo {
            rest: self.rest_config.clone(),
            timeout: Duration::from_secs(15),
        });
        Ok(self)
    }

    /// Routes the queries that bin-ws serves over the session's `info_json`.
    pub fn with_bin_ws(mut self, queries: Arc<dyn InfoTransport>) -> Self {
        self.queries = Some(queries);
        self
    }

    /// Sets how many times a 429 is retried (default 3).
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Runs any `/info` query and returns the reply as JSON. Honours
    /// `Retry-After` on 429.
    pub async fn query(&self, body: &Value) -> Result<Value> {
        let kind = body["type"].as_str().unwrap_or_default();
        let transport = match &self.queries {
            Some(ws) if !REST_ONLY.contains(&kind) => ws,
            _ => &self.rest,
        };
        let bytes = serde_json::to_vec(body).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let mut attempt = 0u32;
        loop {
            match transport.post_info(&bytes).await {
                Ok(reply) => {
                    return serde_json::from_slice(&reply)
                        .map_err(|e| Error::Decode(format!("{kind}: invalid JSON: {e}")));
                }
                Err(e) if e.is_rate_limited() => {
                    let hint = e.retry_after();
                    let wait = hint.unwrap_or_else(|| Duration::from_millis(250 << attempt.min(5)));
                    if attempt >= self.max_retries || wait > self.max_retry_wait {
                        return Err(Error::RateLimited { retry_after: hint });
                    }
                    attempt += 1;
                    tokio::time::sleep(wait).await;
                }
                Err(TransportError::Status { status, body, .. }) => {
                    return Err(Error::Http { status, body })
                }
                Err(other) => return Err(Error::Transport(format!("{kind}: {other:?}"))),
            }
        }
    }

    async fn typed<T: serde::de::DeserializeOwned>(&self, body: Value) -> Result<T> {
        let kind = body["type"].as_str().unwrap_or_default().to_string();
        let value = self.query(&body).await?;
        serde_json::from_value(value).map_err(|e| Error::Decode(format!("{kind}: {e}")))
    }

    /// `chainIdentity`: query it at startup and use `next_agent_chain_id` for signing.
    pub async fn chain_identity(&self) -> Result<ChainIdentity> {
        self.typed(json!({"type": "chainIdentity"})).await
    }

    /// `userRole`: resolve an API wallet to its master. It is expensive
    /// (weight 60): query it at startup only. REST only.
    pub async fn user_role(&self, user: &Address) -> Result<UserRole> {
        let v = self
            .query(&json!({"type": "userRole", "user": user.to_string()}))
            .await?;
        Ok(UserRole::from_value(v))
    }

    /// `userRateLimit`: the request quota of a master account or a vault
    /// (not an API wallet). See [`UserRateLimit::remaining`]. Use it to see
    /// how many requests are left, and to compare before and after an unknown
    /// `reserveRequestWeight` purchase.
    pub async fn user_rate_limit(&self, user: &Address) -> Result<UserRateLimit> {
        let v = self
            .query(&json!({"type": "userRateLimit", "user": user.to_string()}))
            .await?;
        UserRateLimit::from_value(&v)
    }

    /// `leadingVaults`: vaults led by a master account. REST only.
    pub async fn leading_vaults(&self, master: &Address) -> Result<Value> {
        self.query(&json!({"type": "leadingVaults", "user": master.to_string()}))
            .await
    }

    /// `metaAndAssetCtxs`: asset indexes and `szDecimals`. Query it on each
    /// startup. REST only.
    pub async fn meta_and_asset_ctxs(&self) -> Result<MetaAndAssetCtxs> {
        let v = self.query(&json!({"type": "metaAndAssetCtxs"})).await?;
        let parts = v
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or_else(|| Error::Decode("metaAndAssetCtxs: expected [meta, ctxs]".into()))?;
        let meta: Meta = serde_json::from_value(parts[0].clone())
            .map_err(|e| Error::Decode(format!("metaAndAssetCtxs meta: {e}")))?;
        let asset_ctxs = parts[1]
            .as_array()
            .cloned()
            .ok_or_else(|| Error::Decode("metaAndAssetCtxs: ctxs is not an array".into()))?;
        Ok(MetaAndAssetCtxs { meta, asset_ctxs })
    }

    /// `l2Book`: a book snapshot. `nSigFigs` and `mantissa` are not supported
    /// here; use a bin-ws subscription for price-bucketed books.
    pub async fn l2_book(&self, coin: &str) -> Result<L2Book> {
        self.typed(json!({"type": "l2Book", "coin": coin})).await
    }

    /// `openOrders` of a master account or vault.
    pub async fn open_orders(&self, user: &Address) -> Result<Vec<OpenOrder>> {
        self.typed(json!({"type": "openOrders", "user": user.to_string()}))
            .await
    }

    /// `frontendOpenOrders`: also carries cloid, original size and tif.
    pub async fn frontend_open_orders(&self, user: &Address) -> Result<Vec<FrontendOpenOrder>> {
        self.typed(json!({"type": "frontendOpenOrders", "user": user.to_string()}))
            .await
    }

    /// `orderStatus` by oid or by cloid (a cloid query resolves the current
    /// order after a modification).
    pub async fn order_status(&self, user: &Address, target: OrderRef) -> Result<OrderStatusReply> {
        let oid = match target {
            OrderRef::Oid(oid) => json!(oid),
            OrderRef::Cloid(cloid) => json!(cloid.to_string()),
        };
        let v = self
            .query(&json!({"type": "orderStatus", "user": user.to_string(), "oid": oid}))
            .await?;
        OrderStatusReply::from_value(v)
    }

    /// `userFills`, as raw JSON.
    pub async fn user_fills(&self, user: &Address) -> Result<Value> {
        self.query(&json!({"type": "userFills", "user": user.to_string()}))
            .await
    }

    /// `clearinghouseState` (positions and margin), as raw JSON.
    pub async fn clearinghouse_state(&self, user: &Address) -> Result<Value> {
        self.query(&json!({"type": "clearinghouseState", "user": user.to_string()}))
            .await
    }

    /// `candleSnapshot`, as raw JSON.
    pub async fn candle_snapshot(&self, req: &CandleRequest) -> Result<Value> {
        self.query(&json!({"type": "candleSnapshot", "req": {
            "coin": req.coin,
            "interval": req.interval,
            "startTime": req.start_time,
            "endTime": req.end_time,
        }}))
        .await
    }
}

impl std::fmt::Debug for InfoClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InfoClient")
            .field("base", &self.rest_config.base())
            .field("gateway_token", &self.rest_config.has_token())
            .field("bin_ws", &self.queries.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_rate_limit() {
        let v =
            json!({"cumVlm":"25.88","nRequestsCap":10025,"nRequestsSurplus":0,"nRequestsUsed":45});
        let q = UserRateLimit::from_value(&v).unwrap();
        assert_eq!(q.cum_vlm, "25.88");
        assert_eq!(q.remaining(), 10025 - 45);
        // Counts as decimal strings, and the full u64 range.
        let v = json!({"cumVlm":"0","nRequestsCap":"18446744073709551615","nRequestsSurplus":"5","nRequestsUsed":0});
        let q = UserRateLimit::from_value(&v).unwrap();
        assert_eq!(q.n_requests_cap, u64::MAX);
        assert_eq!(q.remaining(), u64::MAX, "saturates");
        // Used above the cap: only the surplus is left (zero here).
        let q = UserRateLimit {
            cum_vlm: "0".into(),
            n_requests_cap: 10,
            n_requests_used: 25,
            n_requests_surplus: 0,
        };
        assert_eq!(q.remaining(), 0);
        let q = UserRateLimit {
            n_requests_used: 0,
            n_requests_surplus: 7,
            ..q
        };
        assert_eq!(q.remaining(), 17);
        assert!(UserRateLimit::from_value(&json!({"cumVlm":"1"})).is_err());
        assert!(UserRateLimit::from_value(
            &json!({"cumVlm":"1","nRequestsCap":-1,"nRequestsSurplus":0,"nRequestsUsed":0})
        )
        .is_err());
    }

    #[test]
    fn parses_order_status_variants() {
        let open = OrderStatusReply::from_value(json!({
            "status": "order", "height": 7,
            "order": {"status": "open", "order": {"oid": 5, "cloid": "0x00000000000000000000000000000001"}}
        }))
        .unwrap();
        match open.state {
            OrderState::Order(o) => {
                assert!(o.is_open() && !o.is_terminal());
                assert_eq!(o.oid, Some(5));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(open.height, Some(7));
        let unknown =
            OrderStatusReply::from_value(json!({"status": "unknownOid", "height": 3})).unwrap();
        assert_eq!(unknown.state, OrderState::UnknownOid);
        let leg = OrderStatusReply::from_value(
            json!({"status": "legResult", "height": 3, "leg": {"resting": {"oid": 9}}}),
        )
        .unwrap();
        assert!(matches!(leg.state, OrderState::LegResult(_)));
        assert!(OrderStatusReply::from_value(json!({"status": "weird"})).is_err());
    }

    #[test]
    fn terminal_statuses_are_known_only() {
        let order = |status: &str| CurrentOrder {
            status: status.into(),
            oid: None,
            cloid: None,
            order: Value::Null,
        };
        assert!(order("filled").is_terminal());
        assert!(order("badAloPxRejected").is_terminal());
        assert!(!order("triggered").is_terminal());
        assert!(!order("somethingNew").is_terminal());
    }

    #[test]
    fn parses_user_roles() {
        assert_eq!(
            UserRole::from_value(json!({"role": "agent", "data": {"user": "0xabc"}})),
            UserRole::Agent {
                user: "0xabc".into()
            }
        );
        assert_eq!(
            UserRole::from_value(json!({"role": "missing"})),
            UserRole::Missing
        );
        assert!(matches!(
            UserRole::from_value(json!({"role": "new"})),
            UserRole::Other(_)
        ));
    }

    #[test]
    fn meta_lookup_uses_position() {
        let meta: Meta = serde_json::from_value(json!({"universe": [
            {"name": "RBT", "szDecimals": 2}, {"name": "BTC", "szDecimals": 5, "maxLeverage": 40}
        ]}))
        .unwrap();
        assert_eq!(meta.asset_index("BTC"), Some(1));
        assert_eq!(meta.asset("BTC").unwrap().sz_decimals, 5);
        assert_eq!(meta.asset_index("NOPE"), None);
    }
}
