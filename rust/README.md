# RiverBit Market Maker SDK for Rust

An async Rust client for market makers integrating with the RiverBit exchange API: signing, order entry, cancellation, modification, market data and reconciliation over REST and the binary WebSocket endpoint (`bin-ws`).

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

An API wallet signs; the master account (or a vault it leads) is the trading account. The master must authorize the API wallet first. Keep the private key out of source control, command-line arguments and logs: the SDK never prints key material, and `Debug` output of a `Signer` shows only its address.

## Quick start

```rust,no_run
use riverbit_mm_sdk::{
    BinWsConfig, BinWsSession, Book, Cloid, Error, Event, ExchangeClient, InfoClient, Order, Place, Signer,
    SigningDomain, Subscription, Tif,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base_url = "https://pre-api.riverbit.io";
    let signer = Signer::from_hex(&std::env::var("RIVERBIT_PRIVATE_KEY")?)?;

    // 1. Startup queries (REST).
    let info = InfoClient::new(base_url)?;
    let chain_id = info.chain_identity().await?.next_agent_chain_id;
    let meta = info.meta_and_asset_ctxs().await?.meta;
    let asset = meta.asset_index("BTC").expect("BTC is listed");

    // 2. Stream the book over bin-ws (the login is the first frame).
    let config = BinWsConfig::new("wss://pre-api.riverbit.io/bin-ws", "pre-api.riverbit.io", signer.clone());
    let (session, mut events) = BinWsSession::connect(config).await?;
    session.subscribe(&Subscription::l2("BTC")).await?;
    let mut book = Book::new("BTC");

    // 3. Trade: the same signed bytes go out over REST or `action_json`.
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
| [`reconcile_after_reconnect`](examples/reconcile_after_reconnect.rs) | Trades over bin-ws; after a reconnect looks up the pending request by cloid |

The examples read `RIVERBIT_PRIVATE_KEY` and, optionally, `RIVERBIT_BASE_URL`, `RIVERBIT_WS_URL`, `RIVERBIT_LOGIN_HOST` and `RIVERBIT_COIN` from the environment. The trading examples send real signed orders to whatever environment they point at.

## Capabilities

| Capability | v0.1 |
|---|---|
| Trading signature: MessagePack in key order, `connectionId`, EIP-712, with and without a vault | Yes |
| bin-ws login signature (EIP-191) | Yes |
| Actions `order`, `cancel`, `cancelByCloid`, `modify`, `batchModify`; `Cloid`; fixed field order; decimal-string prices and sizes | Yes |
| REST `/info` client: `chainIdentity`, `userRole`, `leadingVaults`, `metaAndAssetCtxs`, `l2Book`, `openOrders`, `frontendOpenOrders`, `orderStatus`, `userFills`, `clearinghouseState`, `candleSnapshot`; `Retry-After` honoured on 429 | Yes |
| REST `/exchange` client with per-leg receipts | Yes |
| bin-ws: connect, login first, subscribe, unsubscribe, ping, id-matched `action_json` and `info_json`, reconnect with login and resubscribe | Yes |
| Typed frames `l2` delta, `l2Book`, `bbo`, `trades`, `candle`; all other streams as JSON; typed streams that fall back to JSON | Yes |
| Local order book replaying `l2` snapshots and updates | Yes |
| Per-leg `statuses[]` parsing; rejected, rate-limited and unknown-outcome errors | Yes |
| Reconciliation helper by oid or cloid | Yes |
| Strictly increasing millisecond nonces under concurrency | Yes |
| Durable on-disk pending-request journal with recovery after a process restart | **No** (not in v0.1; keep unknown requests in your own durable store) |
| Quoting strategy, risk limits, automatic cancel on shutdown | No (out of scope) |

## API overview

| Item | Purpose |
|---|---|
| `Signer` | Holds the key. `sign_action`, `sign_request`, `sign_login`, `personal_sign`, `address` |
| `SigningDomain` | `chain_id` (from `chainIdentity.nextAgentChainId`) and agent `source` (`"b"`) |
| `Order`, `Place`, `Cancel` / `CancelItem`, `CancelByCloid` / `CancelByCloidItem`, `Modify`, `BatchModify` / `ModifyItem` | Actions with fixed field order. `Place` is the `order` action and `Order` is one order leg; `CancelItem`, `CancelByCloidItem` and `ModifyItem` are the legs of `cancel`, `cancelByCloid` and `batchModify`. `Place::new` takes `Order`s, `Cancel::new` takes `CancelItem`s, `CancelByCloid::new` takes `CancelByCloidItem`s, `BatchModify::new` takes `ModifyItem`s; `Modify` has one `oid` and one `order`. `Action::to_json` is the exact string that is signed and sent |
| `Cloid` | 128-bit client order ID, `0x` plus 32 hex digits |
| `NonceSource` | Millisecond nonces, strictly increasing; one shared counter per signer in a process |
| `SignedRequest` | A signed request body, nonce and targets. `ExchangeClient::prepare` creates one; `submit` sends it |
| `ExchangeClient` | `send(action)`, `prepare`, `submit`. REST by default, or any `ActionTransport` such as a `BinWsSession`; `for_vault` trades for a vault |
| `InfoClient` | Typed `/info` queries and `query` for any other. Optional routing over bin-ws `info_json` (`with_bin_ws`); `metaAndAssetCtxs`, `userRole` and `leadingVaults` always use REST |
| `BinWsSession`, `BinWsConfig`, `Events`, `Event` | The connection. `subscribe`, `unsubscribe`, `ping`, `info`, `close`; pushes arrive as `Event::Frame` |
| `Subscription` | Constructors for `l2`, `l2Book`, `bbo`, `trades`, `candle`, `orderUpdates`, `openOrders`, `userFills`, `clearinghouseState` and `raw` |
| `Frame` | `L2Delta`, `L2Book`, `Bbo`, `Trades`, `Candle`, or `Json` for every other stream and any typed stream that fell back to JSON |
| `Book` | Local book: `apply_frame` / `apply_delta` / `apply_json`, `best_bid`, `best_ask`, `bids`, `asks` |
| `Receipt`, `LegStatus` | Per-leg result: `Resting`, `Filled`, `Waiting`, `Success`, `Error`, `Unrecognized` |
| `reconcile`, `reconcile_orders`, `OrderReport` | Look up orders by oid or cloid after an unknown outcome or a reconnect |
| `proto` | The hand-written `bin-ws` codec (`ClientFrame`, `ServerFrame` and the market data messages) |

## Errors and unknown outcomes

`Error` separates the outcomes of a write:

| Variant | Meaning | What to do |
|---|---|---|
| `Rejected` | The exchange refused the whole action and nothing executed (for example code 91, 203, 204, 205, 1160, or `PrecisionExceeded`) | Fix the cause, then sign a new request with a new nonce |
| `RateLimited` | A read was throttled and the retry budget ran out | Wait for `retry_after` |
| `UnknownOutcome` | The action may or may not have executed: timeout, disconnect, an HTTP error or 429 on `/exchange`, an unparseable or ambiguous reply | Keep it pending and reconcile. **Never re-sign it with a new nonce** |

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
