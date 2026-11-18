# riverbit-mm-sdk (Python)

Python client library for market makers integrating with the RiverBit exchange API: signing, order entry, cancellation and modification over REST (`POST /exchange`), request quota and request purchases, market data and order and fill events over the binary WebSocket (`bin-ws`), and reconciliation of requests whose outcome is unknown.

> **Status: pre-release.** Interfaces may change before `v0.1.0`. The SDK contains no quoting strategy: it gives you the protocol, signing and reconciliation building blocks to run your own. Read the [protocol guide](../docs/api-guide.md) before you trade.

Repository: <https://github.com/RiverBit-dex/riverbit-mm-sdk>

- Python 3.10 or newer, fully type-annotated (`py.typed`), `asyncio` for all network I/O.
- Signing and encoding are plain synchronous functions.
- Small dependency set: [`coincurve`](https://pypi.org/project/coincurve/) (secp256k1), [`pycryptodome`](https://pypi.org/project/pycryptodome/) (Keccak-256), [`httpx`](https://pypi.org/project/httpx/) (REST) and [`websockets`](https://pypi.org/project/websockets/). No `protoc`, no Protobuf runtime, no MessagePack package: the codecs are hand-written and checked against the golden vectors in [`testdata/`](../testdata/).

## Install

From Git (this library lives in the `python/` subdirectory of the repository):

```sh
pip install "git+https://github.com/RiverBit-dex/riverbit-mm-sdk.git#subdirectory=python"
```

From a checkout, for development:

```sh
cd python
python -m venv .venv && . .venv/bin/activate
pip install -e ".[dev]"
```

## Configuration

Nothing is tied to one environment. You pass the endpoints explicitly:

| Setting | Example (PRE) | Used by |
|---|---|---|
| REST base URL | `https://pre-api.riverbit.io` | `InfoClient`, `ExchangeClient` (`/info`, `/exchange`) |
| WebSocket URL | `wss://pre-api.riverbit.io/bin-ws` | `BinWsSession` |
| Login host | `pre-api.riverbit.io` | `BinWsSession` (the `host:` line of the login message; must match the public server host name) |

Optional: a **gateway credential** issued to you by RiverBit lifts the per-IP REST limits. Pass it as `gateway_token=` to `InfoClient` and `ExchangeClient` (the examples read it from `RIVERBIT_MM_TOKEN`). When set, every REST request to the configured base URL carries it in the `X-RiverBit-MM-Token` header and redirects are not followed; it is never sent over bin-ws, never logged and never shown in `repr` or error messages. An empty value means "not set"; a value with a character that is invalid in an HTTP header (CR, LF, control or non-ASCII characters) is refused at construction. It does not change signing or nonces. The server treats an invalid credential as no credential.

An **API wallet** signs; the **master account** (or a vault it leads) is the trading account. The master must authorize the API wallet first. Keep private keys out of source control, command lines and logs; `Signer` never prints its key.

## Quick start

```python
import asyncio

from riverbit_mm import (
    ActionRejected,
    Cloid,
    ExchangeClient,
    InfoClient,
    Order,
    Signer,
    UnknownOutcome,
    wait_reconciled,
)

BASE_URL = "https://pre-api.riverbit.io"


async def main() -> None:
    signer = Signer(open("key").read().strip())  # one line, 0x-prefixed hex; keep it out of git

    async with InfoClient(BASE_URL) as info, ExchangeClient(BASE_URL, signer) as exchange:
        asset = (await info.assets())["BTC"]  # index and szDecimals: query at startup
        role = await info.user_role(signer.address)  # weight 60: once, at startup
        master = role["data"]["user"] if role["role"] == "agent" else signer.address_lower

        cloid = Cloid.random()
        order = Order(asset.index, True, "50000", "0.001", tif="Alo", cloid=cloid)  # strings!
        try:
            receipt = await exchange.place([order])
        except ActionRejected as exc:  # the whole action was rejected; nothing executed
            print("rejected:", exc.code, exc)
            return
        except UnknownOutcome as exc:  # may have executed: reconcile, never re-sign
            result = await wait_reconciled(info, master, exc, timeout=30)
            print("resolved:", result.resolved, [s.status for s in result.placed])
            return

        for leg in receipt.legs:  # every leg, individually
            print(leg.index, leg.kind, leg.oid, leg.error)


asyncio.run(main())
```

`ExchangeClient` fetches `nextAgentChainId` from `chainIdentity` once, allocates nonces, signs, sends the bytes it signed with `POST /exchange` and parses the receipt. Orders, cancellations and modifications are submitted with REST `POST /exchange`; bin-ws carries market data, order and fill events and `info_json` queries. To stream market data and keep a local book:

```python
from riverbit_mm import BinWsSession, Book, Connected, ServerFrame, subscriptions

async with BinWsSession(
    "wss://pre-api.riverbit.io/bin-ws", "pre-api.riverbit.io", signer
) as session:
    await session.subscribe(subscriptions.l2("BTC"))
    book = Book("BTC")
    async for item in session.frames():
        if isinstance(item, Connected):
            book.reset()  # a (re)connection starts every stream afresh
        elif isinstance(item, ServerFrame) and book.apply(item.body):
            bid, ask = book.best()
            print(bid and bid.px_text, ask and ask.px_text)
```

More in [`examples/`](examples/):

| Example | What it shows |
|---|---|
| [`stream_market_data.py`](examples/stream_market_data.py) | Log in, stream the `l2` book and print the best bid and ask; resync on gaps |
| [`place_query_cancel.py`](examples/place_query_cancel.py) | Place a post-only order with a cloid, query it, cancel it; unknown-outcome handling |
| [`batch_modify.py`](examples/batch_modify.py) | Move two orders with one `batchModify`, read the new oids from the receipt |
| [`quota.py`](examples/quota.py) | Print the `userRateLimit` and the remaining requests; with `BUY_REQUESTS=<n>` also buy `n` requests (signs and sends; without it nothing is signed) |
| [`reconcile_after_reconnect.py`](examples/reconcile_after_reconnect.py) | Rebuild order state after a bin-ws reconnect and reconcile pending requests |

The examples read the key from the file named by `RIVERBIT_KEY_FILE` and the endpoints from `RIVERBIT_*` environment variables (PRE defaults); see [`examples/_common.py`](examples/_common.py). The trading examples send real orders: use a test account.

## Capabilities (v0.1)

| Capability | Status | Notes |
|---|---|---|
| Signing of trading actions (MessagePack in key order, `connectionId`, EIP-712, with and without a vault) | Yes | Reproduces every vector in `testdata/vectors.json` byte for byte |
| bin-ws login signing (EIP-191) | Yes | |
| Actions: `order`, `cancel`, `cancelByCloid`, `modify`, `batchModify`, `reserveRequestWeight`, `Cloid` | Yes | Fixed field order; prices and sizes must be decimal strings |
| REST `/info` client | Yes | `chainIdentity`, `userRole`, `leadingVaults`, `metaAndAssetCtxs`, `l2Book`, `openOrders`, `frontendOpenOrders`, `orderStatus`, `userFills`, `clearinghouseState`, `candleSnapshot`, typed `userRateLimit`, plus `query()` for any other type; `Retry-After` honoured on 429 |
| REST `/exchange` client | Yes | Per-leg receipts; rejection, rate limit and unknown outcome are distinct |
| Request quota: `userRateLimit` query, `remaining` helper, quota rejections told apart, request purchase | Yes | A quota rejection is final; the SDK never retries or buys on its own |
| Optional gateway credential on REST clients | Yes | `X-RiverBit-MM-Token`; never sent over bin-ws |
| bin-ws session | Yes | Login as the first frame, subscribe/unsubscribe, ping, id-matched `info_json` queries, reconnect with login and resubscribe |
| Typed frames | Yes | `l2` (snapshot and update), `l2Book`, `bbo`, `trades`, `candle`; every other stream, and the JSON fallback of typed streams, as `JsonFrame` |
| Local order book | Yes | Replays `l2` snapshots and updates (typed and JSON fallback) per guide section 5 |
| Nonce source | Yes | Millisecond timestamps, strictly increasing under concurrency, shared per signer in a process |
| Unknown-outcome reconciliation helper | Yes | Read-only lookup by oid or cloid; never signs or resends |
| **Durable on-disk pending-request journal with restart recovery** | **No** | Not in v0.1. Persist `SignedRequest` yourself before sending (see below) |
| Automatic quoting or strategy | No | Out of scope |
| Cross-process nonce coordination | No | Run one execution owner per signer |
| Mainnet-specific helpers | No | Pass the environment's endpoints and `chain_id`; `source` defaults to `"b"` |

## API overview

Everything below is importable from `riverbit_mm`.

**Signing** (`riverbit_mm.signing`)

- `Signer(private_key)`: `address`, `address_lower`, `sign_action(action_json, nonce, chain_id=..., vault_address=None) -> SignedRequest`, `sign_login(host, timestamp_ms, nonce=None)`.
- `SignedRequest`: `body` (the exact bytes to `POST` to `/exchange`), `action_json`, `nonce`, `signature`, `vault_address`, `connection_id`, `digest`. The request body is assembled around the very bytes the signature was computed from; nothing is serialised twice.

**Actions** (`riverbit_mm.actions`): `Order`, `Place` (the `order` action), `Cancel` / `CancelItem`, `CancelByCloid` / `CancelByCloidItem`, `Modify`, `BatchModify` / `ModifyItem`, `ReserveRequestWeight`, `Cloid`. Each action has `to_json() -> bytes`. `Place` is the `order` action and `Order` is one order leg; `CancelItem`, `CancelByCloidItem` and `ModifyItem` are the legs of `cancel`, `cancelByCloid` and `batchModify`. Wrap legs in the action: `Place` takes `Order`s, `Cancel` takes `CancelItem`s, `CancelByCloid` takes `CancelByCloidItem`s, `BatchModify` takes `ModifyItem`s; `Modify` takes one oid and one `Order`.

`ReserveRequestWeight(weight, destination=None)` buys `weight` requests (`1 <= weight <= 1_000_000_000`, checked before signing) for 0.0005 USDC each (`reserve_cost_micro_usdc(weight)` is `weight * 500` micro-USDC). `destination` is the account that receives them; absent means the payer. Without a vault the payer is the signer's account (an API wallet pays from its master); to have the master pay for a vault, use a client **without** `vault_address` and set `destination` to the vault. With `vault_address` the vault pays and receives, so `destination` must be absent or equal to the vault (the client refuses anything else before signing); vault-paid purchases may be disabled on a network. A purchase does not consume quota itself.

```python
Place([Order(asset=1, is_buy=True, price="80000", size="0.001", tif="Alo", cloid=Cloid.random())])
```

**Clients**

- `InfoClient(base_url, gateway_token=None)`: one coroutine per query type, `user_rate_limit(user)` (typed `UserRateLimit`, see below), `assets()` (parsed `universe`), `next_agent_chain_id()`, and `query(body)` for anything else.
- `ExchangeClient(base_url, signer, chain_id=None, vault_address=None, gateway_token=None)`: `post(action)` (sign with a fresh nonce and send once), `prepare(action)` / `sign(action)` (sign only, so you can persist the request first), `submit(signed_request)`, and shortcuts `place`, `cancel`, `cancel_by_cloid`, `modify`, `batch_modify`, `reserve_request_weight`. Actions go out with `POST /exchange`.
- `BinWsSession(url, login_host, signer)`: `start()` / `close()` (or `async with`), `subscribe` / `unsubscribe` (recorded and re-sent after a reconnect), `ping`, `post_info`, `query` (unwraps `data`, retries a 429 reply), `frames()` (async iterator), `user`, `signer_address`.
- `riverbit_mm.subscriptions`: builders for `l2`, `l2_book`, `bbo`, `trades`, `candle`, `order_updates`, `user_fills`, `open_orders`, `clearinghouse_state`.

**Request quota**

Each account (the master account, or the vault when trading for a vault; API wallets count against the account they act for) has a cumulative request cap of `10000` plus one request per USD traded. An `order` leg, a `modify`, a `batchModify` entry and a `cancel` / `cancelByCloid` entry cost one request each. Cancels have a higher cap, `min(cap + 100000, 2 * cap)`. Over the cap, one request is still accepted every 10 seconds. Purchased requests raise both caps.

`await info.user_rate_limit(account)` (`account` is the master or a vault, not an API wallet) returns a `UserRateLimit`: `cum_vlm` (decimal string), `requests_cap` (ordinary actions, purchased requests not included), `requests_used` (`max(0, used - purchased)`), `requests_surplus` (`max(0, purchased - used)`; at most one of the two is above zero) and `remaining` (`max(0, cap - used) + surplus`). A node that cannot answer replies HTTP 501, raised as `HttpError`. `parse_user_rate_limit` parses the raw answer; counts may be JSON numbers or decimal strings.

**Frames and book**

- `frames()` yields `ServerFrame(id, body)` with a typed `body` (`L2Delta`, `L2Book`, `Bbo`, `Trades`, `Candle`, or `JsonFrame`), and the session events `Connected(login, reconnect)` and `Disconnected(error, will_reconnect)`. Pongs are consumed internally. A consumer that falls too far behind makes the session drop and re-establish the connection, so you never silently miss book updates.
- Typed prices and sizes are integers divided by `10 ** decimals`; `format_fixed(value, decimals)` renders them exactly (`format_fixed(1010, 1) == "101.0"`).
- `Book(coin).apply(body)` accepts typed `l2` frames and the JSON fallback. It raises `BookError` and leaves the book untouched if a frame cannot be applied (gap, changed decimals): `reset()` it and resubscribe.

**Receipts**: `exchange.post(...)` returns a `Receipt` with `legs` (one `Leg` per `statuses[]` entry: `kind` is `resting`, `filled`, `success`, `waitingForTrigger`, `waitingForFill`, `error` or `unknown`; `oid`, `total_sz`, `avg_px`, `error`), `failed`, `all_ok`, `complete` and `acknowledged` (the bare `{"type":"default"}` answer to a single `modify`). A receipt is only returned for a well-formed answer: statuses that do not match the legs sent (except the single `PrecisionExceeded` status of a multi-leg action, where `complete` is false), or a leg that is malformed or lacks its oid, raise `UnknownOutcome`. A rejected **leg** does not raise: partial success is not rolled back, so inspect every leg and never replay the whole action.

## Errors and unknown outcomes

| Exception | Meaning | What to do |
|---|---|---|
| `ActionRejected` | The node rejected the whole action (`.code`, for example 91, 1160, 203, 204, 97, 1800 for the request quota, 1801 for a purchase overflow). Nothing executed. | Fix the cause, then build a **new** action. The nonce is spent. |
| `RateLimited` | A read was throttled and the retries ran out (`.retry_after`). | Back off. Read clients already retry after `Retry-After`. |
| `UnknownOutcome` | A write may or may not have executed: timeout, disconnect, non-200 (including 429 and 5xx), malformed or incomplete reply (statuses that do not match the legs sent, a leg without its oid), a duplicate nonce (code 205: the first request with that nonce may have executed), or a reply that asks you to query first (`action already pending`, `localCommitted`, ...). Carries `.nonce`, `.request` and `.request_body`. | **Never re-sign it under a new nonce.** Reconcile with `reconcile_unknown` / `wait_reconciled`, and hold conflicting writes until it resolves. |

**Quota rejections** carry the text `Too many cumulative requests sent` (possibly wrapped as `action rejected by execution (code 1800) (...)`, and a cancel leg appends ` asset=N`). They are final, never an unknown outcome:

- per leg (`order`, `batchModify`, `cancel`, `cancelByCloid`): the leg is `kind == "error"` and `leg.quota_exhausted` is true; `receipt.quota_rejected` lists them. An action can be partly accepted: the first legs execute and every later leg carries the quota error;
- whole action (`modify`): `ActionRejected` with `exc.quota_exhausted` true and `exc.code == QUOTA_EXHAUSTED_CODE` (1800), also when the reply carries a `committedResult`;
- `is_quota_error(text)` matches the text by substring; `QUOTA_EXHAUSTED_CODE` is 1800 and `RESERVE_OVERFLOW_CODE` (1801: a purchase would overflow the purchased total, `exc.reserve_overflow`).

Back off (one request per 10 seconds is still accepted), trade, or buy requests. The SDK never retries and never buys on its own.

**A purchase** (`reserveRequestWeight`) has no legs: `ok` with `{"type":"default"}` is a `Receipt` with `acknowledged` true and no `legs`; an `err` reply or any `error` entry is an `ActionRejected`; anything else (timeout, 5xx, malformed) is an `UnknownOutcome`, never re-sent. It has no oid or cloid, so it cannot be looked up: `reconcile_unknown` returns a `Reconciliation` with `resolved` false and `lookup_supported` false (and `wait_reconciled` returns at once). Compare `userRateLimit` (`requests_surplus` and `requests_used`) before and after, and do not buy again until you know.

A 429 on a write is deliberately reported as `UnknownOutcome`, not `RateLimited`: an HTTP status alone does not prove non-execution, and the SDK never retries a write.

`reconcile_unknown(info, account, outcome)` reads `orderStatus` for every oid and cloid the request touches and returns a `Reconciliation`: `targets_settled` is true with positive evidence about what the request names (a placement is found by cloid; a cancellation target is a current order in a terminal state), and `request_final` is true only when the request itself is shown to have run; `resolved` needs both. A **cancel request is never resolved by its target's state**: the target may have ended for another reason while the cancel is still in flight, and a late `cancelByCloid` would cancel a new order that reuses the cloid. Keep such a request pending, and do not reuse a cloid while any request naming it is unresolved (`hold_cloids` lists them; the safest rule is never to reuse one). `unknownOid`, an empty `openOrders` and elapsed time are never treated as proof. **Give every order a cloid**, otherwise it cannot be looked up. `account` is the master address, or the vault address when trading for a vault. `info` can be an `InfoClient` or a `BinWsSession`.

### Pending-request journal (not in v0.1)

The SDK keeps nothing on disk. For restart recovery, a continuous maker should persist each `SignedRequest` (nonce, `body`, account, endpoint) **before** `submit`, and on start reconcile every entry that has no recorded outcome with `reconcile_unknown`. Do not replay the stored body without reconciling first. `ExchangeClient.prepare()` / `submit()` exist so you can place your journal write between signing and sending, and `NonceSource.observe(n)` lets you restore the nonce floor after a restart. See guide section 11.

## Nonces

`NonceSource` returns millisecond timestamps, strictly increasing and thread-safe. `ExchangeClient` uses one shared per signer address in the process; pass your own to coordinate several clients. Deduplication is per signer on the server, and a signer keeps its most recent 100 nonces, so concurrent requests need distinct ones. Nonces cannot be coordinated across processes: use one execution owner per signer.

## Development

```sh
cd python
python -m venv .venv && . .venv/bin/activate
pip install -e ".[dev]"
ruff check . && ruff format --check . && mypy && pytest
```

The test suite reproduces every vector in `testdata/vectors.json` and every frame in `testdata/frames.json`, runs a mock bin-ws server in process, and uses a fake HTTP transport for REST. It needs no network.

## License

[Apache-2.0](../LICENSE)
