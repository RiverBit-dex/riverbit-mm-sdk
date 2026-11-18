# RiverBit Market Maker SDK for Rust

An async Rust client for market makers integrating with the RiverBit exchange API: signing, order entry, cancellation, modification and reconciliation over REST, and market data and order events over the binary WebSocket endpoint (`bin-ws`).

> **Status: pre-release.** Interfaces may change before `v0.1.0`. The SDK contains no quoting strategy: it gives you the protocol, signing and reconciliation building blocks to run your own. Read the [protocol guide](../docs/api-guide.md) before you trade.

Repository: <https://github.com/RiverBit-dex/riverbit-mm-sdk>

- Async on [tokio](https://tokio.rs), TLS via [rustls](https://github.com/rustls/rustls) (no OpenSSL), `#![forbid(unsafe_code)]`.
- No `protoc` needed: the `bin-ws` frames are decoded by a small hand-written codec (see [`proto/binws.proto`](../proto/binws.proto)).
- Minimum supported Rust version: 1.85.

## Install

The crate is not on crates.io yet. Add it as a Git dependency and pin a commit or tag:

```toml
[dependencies]
riverbit-mm-sdk = { git = "https://github.com/RiverBit-dex/riverbit-mm-sdk", rev = "<commit>" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Configuration

Base URL, WebSocket URL and login host are always explicit. The PRE values below are examples; nothing in the library is tied to one environment.

| Setting | PRE example | Used for |
|---|---|---|
| REST base URL | `https://pre-api.riverbit.io` | `POST <base>/info`, `POST <base>/exchange` |
| WebSocket URL | `wss://pre-api.riverbit.io/bin-ws` | `BinWsConfig::url` |
| Login host | `pre-api.riverbit.io` | `host:` line of the login message; must match the public server hostname |
| Chain ID | from `chainIdentity` | `SigningDomain`; never hard-code it |
| Gateway credential (optional) | issued to you by RiverBit | `with_gateway_token` on `InfoClient` and `ExchangeClient`; sent as the `X-RiverBit-MM-Token` header |

An API wallet signs; the master account (or a vault it leads) is the trading account. The master must authorize the API wallet first. Keep the private key out of source control, command-line arguments, environment variables and logs: read it from a file only your process can read. The SDK never prints key material: `Debug` output of a `Signer` shows only its address, and `Debug` output of a `SignedRequest` shows the action kind, nonce and signer but not the body or the signature.

**Gateway credential.** RiverBit can issue a market maker a credential that lifts the per-IP REST limits. Set it with `InfoClient::with_gateway_token` and `ExchangeClient::with_gateway_token`; the examples read it from the optional environment variable `RIVERBIT_MM_TOKEN`. When set, every REST request to the configured base URL carries `X-RiverBit-MM-Token: <credential>`. It is never sent over bin-ws, never logged, and never part of `Debug` or error output. A credential that cannot be an HTTP header value (CR, LF, spaces, non-ASCII) is rejected when you configure it; an empty string means none. It does not change signing, nonces or the taker delay, and an invalid credential is treated by the server as no credential. If you pass your own `reqwest` client, it must not follow redirects.

**Orders are sent with REST `POST /exchange`:** order entry, cancellation and modification. bin-ws carries market data, order and fill events and `info_json` queries.

## Quick start

```rust,no_run
use riverbit_mm_sdk::{
    BinWsConfig, BinWsSession, Book, Cloid, Error, Event, ExchangeClient, InfoClient, Order, Place, Signer,
    SigningDomain, Subscription, Tif,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_url = "https://pre-api.riverbit.io";
    // The key lives in a file that only your process can read.
    let signer = Signer::from_hex(&std::fs::read_to_string(std::env::var("RIVERBIT_KEY_FILE")?)?)?;

    // 1. Startup queries (REST).
    let info = InfoClient::new(base_url)?;
    let chain_id = info.chain_identity().await?.next_agent_chain_id;
    let meta = info.meta_and_asset_ctxs().await?.meta;
    let asset = meta.asset_index("BTC").expect("BTC is listed");

    // 2. Stream the book over bin-ws (market data and events; the login is the first frame).
    let config = BinWsConfig::new("wss://pre-api.riverbit.io/bin-ws", "pre-api.riverbit.io", signer.clone());
    let (session, mut events) = BinWsSession::connect(config).await?;
    session.subscribe(&Subscription::l2("BTC")).await?;
    let mut book = Book::new("BTC");

    // 3. Trade over REST `POST /exchange`.
    let exchange = ExchangeClient::new(base_url, signer, SigningDomain::new(chain_id))?;
    let order = Order::limit(asset, true, "50000", "0.001", Tif::Alo).with_cloid(Cloid::random()?);
    match exchange.send(&Place::new(vec![order])).await {
        Ok(receipt) => println!("legs: {:?}", receipt.legs), // inspect every leg
        Err(Error::Rejected(r)) => println!("rejected, nothing executed: {r}"),
        Err(Error::UnknownOutcome(u)) => println!("unknown outcome, reconcile nonce {}", u.nonce()),
        Err(e) => println!("{e}"),
    }

    while let Some(event) = events.next().await {
        if let Event::Frame(frame) = event {
            if book.apply_frame(&frame)? {
                println!("best bid {:?} ask {:?}", book.best_bid().map(|l| &l.px), book.best_ask().map(|l| &l.px));
            }
        }
    }
    session.close().await;
    Ok(())
}
```

More complete programs are in [`examples/`](examples/):

| Example | What it does |
|---|---|
| [`stream_market_data`](examples/stream_market_data.rs) | Streams the `l2` book and prints the best bid and ask |
| [`place_query_cancel`](examples/place_query_cancel.rs) | Places a post-only order, queries its status, cancels it |
| [`batch_modify`](examples/batch_modify.rs) | Places two orders, moves both with one `batchModify`, cancels them |
| [`quota`](examples/quota.rs) | Prints the request quota (`userRateLimit`) and the remaining requests; with `BUY_REQUESTS=<n>` it also buys requests and prints the quota again. Without that variable nothing is signed |
| [`reconcile_after_reconnect`](examples/reconcile_after_reconnect.rs) | Sends an order over REST, follows order events over bin-ws and looks up a pending request by cloid, including after a reconnect |

The examples read the key from the file named by `RIVERBIT_KEY_FILE` (for example `RIVERBIT_KEY_FILE=./key cargo run --example place_query_cancel`; keep that file out of source control) and, optionally, `RIVERBIT_BASE_URL`, `RIVERBIT_WS_URL`, `RIVERBIT_LOGIN_HOST` and `RIVERBIT_COIN` from the environment. All examples send orders over REST. The trading examples send real signed orders to whatever environment they point at.

## Capabilities

| Capability | v0.1 |
|---|---|
| Trading signature: MessagePack in key order, `connectionId`, EIP-712, with and without a vault | Yes |
| bin-ws login signature (EIP-191) | Yes |
| Actions `order`, `cancel`, `cancelByCloid`, `modify`, `batchModify`, `reserveRequestWeight` (buy requests); `Cloid`; fixed field order; decimal-string prices and sizes | Yes |
| REST `/info` client: `chainIdentity`, `userRole`, `leadingVaults`, `metaAndAssetCtxs`, `l2Book`, `openOrders`, `frontendOpenOrders`, `orderStatus`, `userFills`, `clearinghouseState`, `candleSnapshot`, `userRateLimit`; `Retry-After` honoured on 429 | Yes |
| REST `/exchange` client with per-leg receipts. Never follows redirects | Yes |
| bin-ws: connect, login first, subscribe, unsubscribe, ping, id-matched `info_json`, reconnect with login and resubscribe | Yes |
| Typed frames `l2` delta, `l2Book`, `bbo`, `trades`, `candle`; all other streams as JSON; typed streams that fall back to JSON | Yes |
| Local order book replaying `l2` snapshots and updates | Yes |
| Per-leg `statuses[]` parsing; rejected, rate-limited and unknown-outcome errors | Yes |
| Reconciliation helper by oid or cloid (a `reserveRequestWeight` purchase has no order: compare `userRateLimit` before and after) | Yes |
| Request quota: typed `userRateLimit` with `remaining()`; quota rejections recognized per leg and per action as final rejections (codes 1800 and 1801) | Yes |
| Optional gateway credential on the REST clients (`X-RiverBit-MM-Token`) | Yes |
| Strictly increasing millisecond nonces under concurrency | Yes |
| Durable on-disk pending-request journal with recovery after a process restart | **No** (not in v0.1; keep unknown requests in your own durable store) |
| Quoting strategy, risk limits, automatic cancel on shutdown | No (out of scope) |

## API overview

| Item | Purpose |
|---|---|
| `Signer` | Holds the key. `sign_action`, `sign_request`, `sign_login`, `personal_sign`, `address` |
| `SigningDomain` | `chain_id` (from `chainIdentity.nextAgentChainId`) and agent `source` (`"b"`) |
| `Order`, `Place`, `Cancel` / `CancelItem`, `CancelByCloid` / `CancelByCloidItem`, `Modify`, `BatchModify` / `ModifyItem` | Actions with fixed field order. `Place` is the `order` action and `Order` is one order leg; `CancelItem`, `CancelByCloidItem` and `ModifyItem` are the legs of `cancel`, `cancelByCloid` and `batchModify`. `ReserveRequestWeight` is the `reserveRequestWeight` action (`weight`, optional `destination`; `cost_micro_usdc`). `Place::new` takes `Order`s, `Cancel::new` takes `CancelItem`s, `CancelByCloid::new` takes `CancelByCloidItem`s, `BatchModify::new` takes `ModifyItem`s; `Modify` has one `oid` and one `order`. `Action::to_json` is the exact string that is signed and sent |
| `Cloid` | 128-bit client order ID, `0x` plus 32 hex digits |
| `NonceSource` | Millisecond nonces, strictly increasing; one shared counter per signer in a process |
| `SignedRequest` | A signed request body, nonce and targets. `ExchangeClient::prepare` creates one; `submit` sends it |
| `ExchangeClient` | `send(action)`, `prepare`, `submit`, `with_gateway_token`. REST `POST /exchange`; `for_vault` trades for a vault |
| `InfoClient` | Typed `/info` queries (including `user_rate_limit` returning `UserRateLimit`) and `query` for any other. `with_gateway_token` sets the optional credential. Optional routing over bin-ws `info_json` (`with_bin_ws`); `metaAndAssetCtxs`, `userRole` and `leadingVaults` always use REST |
| `BinWsSession`, `BinWsConfig`, `Events`, `Event` | The connection. `subscribe`, `unsubscribe`, `ping`, `info`, `close`; pushes arrive as `Event::Frame` |
| `Subscription` | Constructors for `l2`, `l2Book`, `bbo`, `trades`, `candle`, `orderUpdates`, `openOrders`, `userFills`, `clearinghouseState` and `raw` |
| `Frame` | `L2Delta`, `L2Book`, `Bbo`, `Trades`, `Candle`, or `Json` for every other stream and any typed stream that fell back to JSON |
| `Book` | Local book: `apply_frame` / `apply_delta` / `apply_json`, `best_bid`, `best_ask`, `bids`, `asks` |
| `Receipt`, `LegStatus` | Per-leg result: `Resting`, `Filled`, `Waiting`, `Success`, `Error`, `Unrecognized`. `Receipt::quota_rejected_legs`, `LegStatus::is_quota_error` |
| `reconcile`, `reconcile_orders`, `OrderReport` | Look up orders by oid or cloid after an unknown outcome or a reconnect |
| `proto` | The hand-written `bin-ws` codec (`ClientFrame`, `ServerFrame` and the market data messages) |

## Errors and unknown outcomes

`Error` separates the outcomes of a write:

| Variant | Meaning | What to do |
|---|---|---|
| `Rejected` | The exchange refused the whole action and nothing executed (for example code 91, 203, 204, 1160, 1800 or 1801, or one `PrecisionExceeded` status for a multi-leg action). `Error::is_quota_exhausted` and `Rejected::is_quota_exhausted` tell a quota rejection (code 1800) from the others; `Rejected::is_purchase_overflow` is code 1801 | Fix the cause, then sign a new request with a new nonce |
| `RateLimited` | A read was throttled and the retry budget ran out | Wait for `retry_after` |
| `UnknownOutcome` | The action may or may not have executed: timeout, disconnect, an HTTP error, a redirect or 429 on `/exchange`, an unparseable or ambiguous reply, a duplicate nonce (code 205), `action already pending`, or a receipt whose statuses do not match the submitted legs | Keep it pending and reconcile. **Never re-sign it with a new nonce** |

An `UnknownOutcome` carries the signed request: nonce, the exact bytes sent, the oids and cloids the action referred to, the reason and any legs that were observed. Submitting the same `SignedRequest` again is safe (a duplicate nonce cannot execute twice), but the reply to a duplicate does not tell you what the original did. Reconcile instead:

```rust,no_run
# use riverbit_mm_sdk::{reconcile, Address, InfoClient, UnknownOutcome};
# async fn run(info: InfoClient, master: Address, unknown: UnknownOutcome) -> riverbit_mm_sdk::Result<()> {
for report in reconcile(&info, &master, &unknown).await? {
    println!("{:?}: {:?}", report.target, report.reply.state);
}
# Ok(()) }
```

`unknownOid`, an empty open-order list, an elapsed timeout and a consumed nonce are not proof that nothing executed. Orders without a cloid cannot be looked up this way; compare `frontend_open_orders` and `userFills`. `Ok(Receipt)` means the exchange processed the action, not that every leg succeeded: inspect `Receipt::legs`. See sections 6 to 11 of the [protocol guide](../docs/api-guide.md).

## Request quota

Each account (the master account, or the vault when you trade for a vault; API wallets count against the account they act for) has a cumulative request cap that grows with traded volume. `order` uses one request per leg, `modify` one, `batchModify` one per entry, `cancel` and `cancelByCloid` one per entry. Cancels have a higher cap. Once an account is over its cap, one request is accepted every 10 seconds. Read the state with `InfoClient::user_rate_limit` (master account or vault address, not an API wallet); `UserRateLimit::remaining` is `max(0, cap - used) + surplus`. A node that cannot answer replies HTTP 501, reported as `Error::Http`.

A quota rejection (text containing `Too many cumulative requests sent`, code 1800) is **final**, not an unknown outcome: nothing executed for that leg or action.

- For `order`, `batchModify`, `cancel` and `cancelByCloid` it arrives per leg: the action can be partly accepted, so the first legs execute and every later leg carries the error. It is in `Receipt::legs` as `LegStatus::Error`; `Receipt::quota_rejected_legs` lists the indexes and `LegStatus::is_quota_error` tests one leg.
- For `modify` it is a whole-action rejection: `Err(Error::Rejected)` with `Error::is_quota_exhausted() == true`. The original text stays in `Rejected::message`.

The SDK never retries or buys on its own. Back off (one request per 10 seconds is still accepted), trade to free up requests, or buy requests.

`ReserveRequestWeight::new(n)` buys `n` requests (`1..=1_000_000_000`, checked before signing) at 0.0005 USDC each (`cost_micro_usdc`), paid by the payer. It is signed and sent exactly like `order`, and does not consume quota itself. Without a vault the payer is the signer's account (for an API wallet, its master); `with_destination` sends the requests to another account, so a master can pay for a vault by using a client **without** `for_vault` and setting the destination to the vault. With a `for_vault` client the vault pays and receives (the destination must be absent or the vault; vault-paid purchases may be disabled on a network). Success is `Receipt::default_ack` with no legs; a rejection (including an overflow, code 1801) is `Error::Rejected`.

```rust,no_run
# use riverbit_mm_sdk::{Error, ExchangeClient, InfoClient, ReserveRequestWeight};
# async fn run(info: InfoClient, exchange: ExchangeClient, master: riverbit_mm_sdk::Address) -> riverbit_mm_sdk::Result<()> {
let before = info.user_rate_limit(&master).await?;
match exchange.send(&ReserveRequestWeight::new(1000)).await {
    Ok(receipt) => assert!(receipt.default_ack),
    Err(Error::Rejected(r)) => println!("nothing bought: {r}"),
    Err(Error::UnknownOutcome(u)) => {
        // No order to look up, and never sign it again: compare the quota.
        let after = info.user_rate_limit(&master).await?;
        println!("nonce {}: surplus {} -> {}", u.nonce(), before.n_requests_surplus, after.n_requests_surplus);
    }
    Err(e) => println!("{e}"),
}
# Ok(()) }
```

`reconcile` on the unknown outcome of a purchase returns `Error::InvalidInput` (there is no oid or cloid): an unknown purchase is resolved by comparing `nRequestsSurplus` and `nRequestsUsed` before and after.

After `Event::Reconnected`, log in and resubscribe have already happened; rebuild your own state (open orders, pending requests) before acting on updates.

## Tests

```text
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

The tests reproduce every entry of [`testdata/vectors.json`](../testdata/vectors.json) byte for byte (typed actions, MessagePack image, `connectionId`, digest, signature, login signature), decode and re-encode every frame of [`testdata/frames.json`](../testdata/frames.json) and replay its `l2` frames into a book, and run the REST and bin-ws clients against in-process servers. No test touches the network.

## License

[Apache-2.0](../LICENSE)
