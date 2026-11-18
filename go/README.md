# RiverBit market maker SDK for Go

Go client library for the RiverBit exchange API: signing, order entry over REST, cancellation, modification, market data over the binary WebSocket (`bin-ws`), and reconciliation of requests whose outcome is unknown.

> **Status: pre-release.** Interfaces may change before `v0.1.0`. The SDK contains no quoting strategy: it gives you the protocol, signing and reconciliation building blocks to run your own. Read the [protocol guide](../docs/api-guide.md) before you trade.

Repository: <https://github.com/RiverBit-dex/riverbit-mm-sdk>

## Install

```sh
go get github.com/riverbit-dex/riverbit-mm-sdk/go@latest
```

Requires Go 1.24 or newer. The module path ends in `/go` because the repository holds one library per language.

## Packages

| Package | Contents |
|---|---|
| [`exchange`](exchange/) | `Signer`, action types, `Client` (signing, REST submission, receipts, durable pending journal, recovery), `InfoClient` (REST `/info`), error types |
| [`binws`](binws/) | bin-ws frames and codec, login, `Session`, reconnecting `Run`, `Router` for typed frames, `Book` for the incremental `l2` stream |
| [`binws/binwstest`](binws/binwstest/) | In-process fake bin-ws server for your own tests |
| [`examples/mmcli`](examples/mmcli/) | Example command-line client (market data, `trade`, `recover`, `quota`) built only on the packages above |

## Quick start

Watch a book over bin-ws, then place one order over REST. An API wallet signs; the master account (or a vault it leads) is the trading account, and the master must authorize the API wallet first.

```go
package main

import (
	"context"
	"errors"
	"fmt"
	"log"

	"github.com/riverbit-dex/riverbit-mm-sdk/go/binws"
	"github.com/riverbit-dex/riverbit-mm-sdk/go/exchange"
)

func main() {
	ctx := context.Background()
	const (
		restURL   = "https://pre-api.riverbit.io"
		wsURL     = "wss://pre-api.riverbit.io/bin-ws"
		loginHost = "pre-api.riverbit.io"
	)
	signer, err := exchange.NewSigner(readKey()) // load the key from a file or a secret store, not from source
	if err != nil {
		log.Fatal(err)
	}

	// Unsigned queries: asset index and precision come from metadata, never from constants.
	info := exchange.NewInfoClient(restURL)
	btc, err := info.Asset(ctx, "BTC")
	if err != nil {
		log.Fatal(err)
	}

	// One authenticated bin-ws session carries market data and the reconciliation queries.
	router := &binws.Router{
		Book: binws.NewBook("BTC"),
		OnBook: func(b *binws.Book, snapshot bool) {
			if bid, ask := b.Best(); bid != nil && ask != nil {
				fmt.Println("best bid", bid.PxText, "best ask", ask.PxText)
			}
		},
	}
	session, err := binws.Dial(ctx, binws.Config{URL: wsURL, LoginHost: loginHost, Sign: signer, OnFrame: router.Handle})
	if err != nil {
		log.Fatal(err)
	}
	defer session.Close()
	_ = session.Subscribe(binws.L2("BTC"))

	// The trading client. Orders go out as REST POST /exchange. StateDir turns
	// on the durable pending journal.
	client, err := exchange.New(restURL, signer)
	if err != nil {
		log.Fatal(err)
	}
	client.Queries = session  // reconciliation queries as info_json (optional; REST /info otherwise)
	client.StateDir = "state" // one directory per signer
	if err := client.SetPrincipal(session.User()); err != nil { // the master the login resolved
		log.Fatal(err)
	}
	if err := client.LoadChainID(ctx); err != nil { // nextAgentChainId, required for signing
		log.Fatal(err)
	}
	if err := client.RecoverPending(ctx); err != nil { // resolve what an earlier run left unknown
		log.Fatal(err)
	}

	// A fresh cloid per order. Never reuse a cloid while any earlier request
	// that carried it is unresolved: a lookup by cloid cannot tell the orders apart.
	order := exchange.LimitOrder(btc.Index, true, "80000", "0.001", "Alo")
	if order.Cloid, err = exchange.NewCloid(); err != nil {
		log.Fatal(err)
	}
	var partial *exchange.PartialExecutionError
	switch err := client.Exchange(ctx, exchange.NewPlace(order)); {
	case err == nil:
		fmt.Println("placed, order IDs", client.LastOrderIDs)
	case errors.Is(err, exchange.ErrExecutionRejected):
		fmt.Println("rejected, no leg executed:", err) // final; fix the cause, a new request gets a new nonce
	case errors.As(err, &partial):
		fmt.Println("some legs ran:", partial.Executed, "rejected:", partial.Rejected, "to track:", partial.Tracking)
	default:
		fmt.Println("outcome unknown:", err) // reconcile with RecoverPending; never resend
	}
}
```

Orders, cancellations and modifications are submitted with REST `POST /exchange`. bin-ws carries market data, order and fill events, and `info_json` queries.

## Capabilities

| Capability | Where | Notes |
|---|---|---|
| Action signing (EIP-712, MessagePack key order, with or without a vault) | `exchange.Client.Sign`, `SignedRequest` | Signed bytes and sent bytes are the same bytes. Accepts only the SDK action types (or the result of `ParseAction` for raw JSON); maps and `json.RawMessage` are refused. Reproduces every vector in `testdata/vectors.json` byte for byte |
| bin-ws login signing (EIP-191) | `exchange.Signer.PersonalSign`, `binws.NewLogin` | `binws.Signer` is an interface, so a remote or hardware key can sign |
| Actions: `order`, `cancel`, `cancelByCloid`, `modify`, `batchModify`, `reserveRequestWeight` | `exchange.Place`, `Cancel`, `CancelByCloid`, `Modify`, `BatchModify`, `ReserveRequestWeight`; `NewPlace`, `LimitOrder`, `NewReserveRequestWeight`, ... | Prices and sizes are decimal strings end to end. `exchange.Cloid` is a 128-bit client order ID. `weight` is an integer JSON number, validated to 1..1000000000 before signing |
| REST `/info` | `exchange.InfoClient` | `Query` for any type; typed helpers for `chainIdentity`, `metaAndAssetCtxs`, `userRole`, `openOrders`, `userRateLimit` (`UserRateLimit.Remaining`). 429 is retried: it waits for `Retry-After` when present, otherwise capped exponential backoff |
| REST `/exchange` | `exchange.Client` | Orders, cancellations and modifications. Redirects are never followed: a 3xx answer is an unknown outcome |
| bin-ws session | `binws.Dial`, `Session` | First-frame login, subscribe/unsubscribe, ping, `info_json` requests matched by frame id |
| Reconnect | `binws.Run` | Reconnect, log in again, resubscribe, with backoff |
| Typed frames | `binws.Router` | `l2`, `l2Book`, `bbo`, `trades`, `candle`; other streams arrive as JSON; the JSON fallbacks of typed streams are handled |
| Local order book | `binws.Book` | Replays `l2` snapshots and updates (typed and JSON), exact `big.Rat` levels |
| Receipts | `exchange.ParseReceipt`, `Client.LastReceipt` | Per-leg `resting`, `filled`, `error`, ... A receipt must carry one well-formed status per leg, with an order ID on `resting` and `filled`; a short or malformed one is an unknown outcome |
| Error classes | `*exchange.ActionRejectedError`, `ErrExecutionRejected`, `*exchange.PartialExecutionError`, `*exchange.RateLimitError`, `*exchange.UnknownOutcomeError`, `ErrRequestQuotaExceeded` (`IsQuotaRejection`) | Rejection of every leg, partial execution, rate limit and unknown outcome are distinct types. `ErrExecutionRejected` means no leg executed; when some legs ran and others were rejected or wait for a fill or trigger, `PartialExecutionError` lists them per leg. A duplicate nonce (code 205) is an unknown outcome: the original may have executed. A request quota rejection (code 1800, text contains `Too many cumulative requests sent`) is final: `exchange.IsQuotaRejection(err)`, `errors.Is(err, exchange.ErrRequestQuotaExceeded)`, `LegStatus`/`LegOutcome`/`Receipt.IsQuotaRejection()`; the original text stays available |
| Request quota | `InfoClient.UserRateLimit`, `Client.UserRateLimit` | Cap is 10000 + 1 per USD traded (cancels: `min(cap + 100000, 2 * cap)`); purchased requests add to both. Units: one per order leg, per modify, per batchModify entry, per cancel entry. Over the cap one unit is accepted every 10 seconds. The SDK never retries or buys on its own |
| Buying requests | `exchange.ReserveRequestWeight` through `Client.Exchange` | 0.0005 USDC per request (`ReserveCostMicroUSDC`), paid by the payer: the signer's account (the master for an API wallet), or the vault for a `NewForVault` client (a per-network setting; `Destination` must then be empty or the vault). To have the master pay for a vault, use a client without a vault and `NewReserveRequestWeightFor(n, vault)`. Success is `ok` with `{"type":"default"}`; overflow (code 1801) is a final rejection |
| Gateway credential | `Client.SetGatewayCredential`, `InfoClient.SetGatewayCredential` | Optional. Adds `X-RiverBit-MM-Token` to REST `/info` and `/exchange` requests, never to bin-ws or to another URL. Never logged, journaled or printed; a value with a CR, LF or non-ASCII character is refused. It does not change signing, nonces or the taker delay |
| Nonces | `exchange.Client` | Millisecond timestamps, unique per signer in the process, persisted as a floor in `StateDir` |
| **Durable pending journal and `recover`** | `Client.StateDir`, `RecoverPending` | Every request is journaled before it is sent. After a timeout, disconnect or restart it is resolved by order ID or client order ID, never re-signed with a new nonce. A pending purchase has no order to look up: it stays pending and is never resent (`ErrPurchaseUnresolved`); compare `userRateLimit` before and after, then `AcknowledgePendingPurchase`. Audit log and execution-height floor included. Needs a Unix-like OS (advisory file lock) |
| Fake bin-ws server | `binws/binwstest` | Real login check, scripted replies |

Not included: quoting or risk logic, automatic cancellation on shutdown, a cross-host nonce allocator (one process, or processes sharing one `StateDir` on one host, may own a signer), `l2Book` price bucketing over REST.

## API overview

- Action types: `Place` is the `order` action and `Order` is one order leg; `CancelItem`, `CancelByCloidItem` and `ModifyItem` are the legs of `cancel`, `cancelByCloid` and `batchModify`; `ReserveRequestWeight` buys request quota and has no legs. Constructors: `NewPlace(orders...)`, `NewCancel(items...)`, `NewCancelByCloid(items...)`, `NewModify(oid, order)`, `NewBatchModify(items...)`, `NewReserveRequestWeight(n)`, `NewReserveRequestWeightFor(n, account)`.
- `exchange.NewSigner(hexKey)` loads a key. `exchange.New(url, signer)` trades for the signer's own account; `NewForVault(url, signer, vault)` for a vault; `SetPrincipal(master)` records the master an API wallet acts for. Call `VerifySubAccount` before trading for a vault.
- Set `SignChainID` through `Client.LoadChainID`; there is no default, and a wrong value gives code 91.
- `Client.Exchange(ctx, action)` signs, sends and reconciles. `LastOrderIDs`, `LastStatuses` and `LastReceipt` describe the last request. `RecoverPending(ctx)` only observes: it queries and never submits.
- Orders are submitted over REST `POST /exchange`. Set `Client.Queries` to a `*binws.Session` to run the reconciliation queries over bin-ws `info_json`; otherwise they use REST `/info`.
- Use `exchange.NewCloid()` for a fresh client order ID per order. A cloid must not be reused while any earlier request with it is unresolved: recovery accepts an order found by cloid only if it was created after the request was journaled and has the submitted side, price and size, and otherwise keeps the request pending. When the reply carried a receipt, recovery follows the order IDs in it instead.
- `binws.Config.OnFrame` receives pushes; `Router.Handle` is a ready callback. `Config.Sign` takes an `exchange.Signer`.
- Quota: `info.UserRateLimit(ctx, master)` (or `client.UserRateLimit(ctx)` for the client's account) returns the counts as `uint64`, `CumVlm` as a decimal string, and `Remaining()`, which is `max(0, Cap - Used) + Surplus`. A quota rejection is final: back off (one unit per 10 seconds is still accepted), trade, or buy. `client.Exchange(ctx, exchange.NewReserveRequestWeight(100000))` buys 100000 requests; the receipt of a purchase has no legs, so the per-leg checks do not apply to it.
- Gateway credential: `client.SetGatewayCredential(token)` (and the same on `InfoClient`), optional. The examples read it from `RIVERBIT_MM_TOKEN`.
- Errors: `errors.As` into `*exchange.UnknownOutcomeError` to get the nonce, account, signed action and cloids to reconcile; `errors.Is(err, exchange.ErrExecutionRejected)` for a final rejection in which no leg executed; `errors.As` into `*exchange.PartialExecutionError` for a request in which some legs executed (`Executed`), some were rejected (`Rejected`) and some wait (`Tracking`). Partial success is not rolled back: never resend the whole request.

Run `go doc github.com/riverbit-dex/riverbit-mm-sdk/go/exchange` (and `.../binws`) for the full reference.

## Examples

`examples/mmcli` reads its settings from environment variables and the key from a file (`KEY_FILE`, default `key`; keep it out of source control). From this directory:

```sh
# Market data: book, trades, candles (and order events for the logged-in account)
KEY_FILE=./key go run ./examples/mmcli

# Submit the actions in ACTION_FILE (comma-separated) and reconcile each one
ACTION_FILE=examples/mmcli/actions/btc-batch-order.sample.json KEY_FILE=./key go run ./examples/mmcli trade

# Resolve a request that was left unknown by an earlier run; never submits
KEY_FILE=./key go run ./examples/mmcli recover

# Print the request quota (nothing signed is sent)
KEY_FILE=./key go run ./examples/mmcli quota

# Buy 100000 requests, print the receipt and the quota after
BUY_REQUESTS=100000 KEY_FILE=./key go run ./examples/mmcli quota
```

`quota` sends nothing signed unless `BUY_REQUESTS` is set. A purchase of unknown outcome stays pending and is never resent: compare the quota printed before and after, then clear it with `ACK_PURCHASE_NONCE=<nonce> mmcli recover`.

| Variable | Default | Meaning |
|---|---|---|
| `BIN_WS_URL` | `wss://pre-api.riverbit.io/bin-ws` | bin-ws endpoint |
| `BIN_WS_LOGIN_HOST` | `pre-api.riverbit.io` | `host:` line of the login message; must equal the server's public host |
| `API_GATEWAY_URL` | `https://pre-api.riverbit.io` | REST base URL (`/exchange` for orders, `/info` for queries) |
| `COIN` | `BTC` | market to watch and trade |
| `BOOK_STREAM` | `l2` | `l2` (incremental) or `l2Book` (snapshots) |
| `GUILD_VAULT_ADDRESS` | empty | trade for this vault; the signer must lead it or be its API wallet |
| `KEY_FILE` | `key` | file with the private key |
| `STATE_DIR` | `state` | durable pending journal directory |
| `SIGN_CHAIN_ID` | empty | if set, must equal `chainIdentity` |
| `ACTION_FILE` | none | action JSON files for `trade` |
| `BUY_REQUESTS` | none | number of requests `quota` buys (1 to 1000000000); unset means read only |
| `ACK_PURCHASE_NONCE` | none | with `recover`: clear the pending purchase with this nonce after comparing the quota; sends nothing |
| `RIVERBIT_MM_TOKEN` | none | optional gateway credential, sent as `X-RiverBit-MM-Token` on REST requests |

The sample actions use asset index 1 and placeholder prices; `trade` checks them against the asset index of `COIN` from `metaAndAssetCtxs` and refuses to send a mismatch. Orders left by `trade` stay on the book until you cancel them; the examples never cancel automatically.

## Tests

From this directory:

```sh
gofmt -l .
go vet ./...
go test -race ./...
```

The tests need no network and no keys. `exchange` and `binws` check the shared files `../testdata/vectors.json` (signing and login vectors) and `../testdata/frames.json` (bin-ws frames) byte for byte.

## License

[Apache-2.0](../LICENSE)
