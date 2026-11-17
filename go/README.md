# RiverBit market maker SDK for Go

Go client library for the RiverBit exchange API: signing, order entry, cancellation, modification, market data over the binary WebSocket (`bin-ws`), and reconciliation of requests whose outcome is unknown.

> **Status: pre-release.** Interfaces may change before `v0.1.0`. The library contains no quoting strategy. Read the [protocol guide](../docs/api-guide.md) first; this README only shows how the Go packages map onto it.

## Install

```sh
go get github.com/riverbit-dex/riverbit-mm-sdk/go@latest
```

Requires Go 1.26 or newer. The module path ends in `/go` because the repository holds one library per language.

## Packages

| Package | Contents |
|---|---|
| [`exchange`](exchange/) | `Signer`, action types, `Client` (signing, REST and bin-ws submission, receipts, durable pending journal, recovery), `InfoClient` (REST `/info`), error types |
| [`binws`](binws/) | bin-ws frames and codec, login, `Session`, reconnecting `Run`, `Router` for typed frames, `Book` for the incremental `l2` stream |
| [`binws/binwstest`](binws/binwstest/) | In-process fake bin-ws server for your own tests |
| [`examples/mmcli`](examples/mmcli/) | Example command-line client (market data, `trade`, `recover`) built only on the packages above |

## Quick start

Watch a book over bin-ws, then place and cancel one order. An API wallet signs; the master account (or a vault it leads) is the trading account, and the master must authorize the API wallet first.

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

	// One authenticated bin-ws session carries market data and signed actions.
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

	// The trading client. StateDir turns on the durable pending journal.
	client, err := exchange.New(restURL, signer)
	if err != nil {
		log.Fatal(err)
	}
	client.WS, client.Queries = session, session // actions as action_json, reconciliation as info_json
	client.StateDir = "state"                    // one directory per signer
	if err := client.SetPrincipal(session.User()); err != nil { // the master the login resolved
		log.Fatal(err)
	}
	if err := client.LoadChainID(ctx); err != nil { // nextAgentChainId, required for signing
		log.Fatal(err)
	}
	if err := client.RecoverPending(ctx); err != nil { // resolve what an earlier run left unknown
		log.Fatal(err)
	}

	order := exchange.LimitOrder(btc.Index, true, "80000", "0.001", "Alo")
	order.Cloid = "0x00000000000000000000000000000001"
	switch err := client.Exchange(ctx, exchange.NewPlace(order)); {
	case err == nil:
		fmt.Println("placed, order IDs", client.LastOrderIDs)
	case errors.Is(err, exchange.ErrExecutionRejected):
		fmt.Println("rejected:", err) // final; fix the cause, a new request gets a new nonce
	default:
		fmt.Println("outcome unknown:", err) // reconcile with RecoverPending; never resend
	}
}
```

## Capabilities

| Capability | Where | Notes |
|---|---|---|
| Action signing (EIP-712, MessagePack key order, with or without a vault) | `exchange.Client.Sign`, `SignedRequest` | Signed bytes and sent bytes are the same bytes. Reproduces every vector in `testdata/vectors.json` byte for byte |
| bin-ws login signing (EIP-191) | `exchange.Signer.PersonalSign`, `binws.NewLogin` | `binws.Signer` is an interface, so a remote or hardware key can sign |
| Actions: `order`, `cancel`, `cancelByCloid`, `modify`, `batchModify` | `exchange.Place`, `Cancel`, `CancelByCloid`, `Modify`, `BatchModify`; `NewPlace`, `LimitOrder`, ... | Prices and sizes are decimal strings end to end. `exchange.Cloid` is a 128-bit client order ID |
| REST `/info` | `exchange.InfoClient` | `Query` for any type; typed helpers for `chainIdentity`, `metaAndAssetCtxs`, `userRole`, `openOrders`. 429 is retried honoring `Retry-After` |
| REST `/exchange` | `exchange.Client` | One request body for REST and bin-ws |
| bin-ws session | `binws.Dial`, `Session` | First-frame login, subscribe/unsubscribe, ping, `action_json` and `info_json` requests matched by frame id |
| Reconnect | `binws.Run` | Reconnect, log in again, resubscribe, with backoff |
| Typed frames | `binws.Router` | `l2`, `l2Book`, `bbo`, `trades`, `candle`; other streams arrive as JSON; the JSON fallbacks of typed streams are handled |
| Local order book | `binws.Book` | Replays `l2` snapshots and updates (typed and JSON), exact `big.Rat` levels |
| Receipts | `exchange.ParseReceipt`, `Client.LastReceipt` | Per-leg `resting`, `filled`, `error`, ... |
| Error classes | `*exchange.ActionRejectedError`, `ErrExecutionRejected`, `*exchange.RateLimitError`, `*exchange.UnknownOutcomeError` | Whole-action rejection, rate limit and unknown outcome are distinct types; per-leg rejections are in the receipt |
| Nonces | `exchange.Client` | Millisecond timestamps, unique per signer in the process, persisted as a floor in `StateDir` |
| **Durable pending journal and `recover`** | `Client.StateDir`, `RecoverPending` | Every request is journaled before it is sent. After a timeout, disconnect or restart it is resolved by order ID or client order ID, never re-signed with a new nonce. Audit log and execution-height floor included. Needs a Unix-like OS (advisory file lock) |
| Fake bin-ws server | `binws/binwstest` | Real login check, scripted replies |

Not included: quoting or risk logic, automatic cancellation on shutdown, a cross-host nonce allocator (one process, or processes sharing one `StateDir` on one host, may own a signer), `l2Book` price bucketing over REST.

## API overview

- `exchange.NewSigner(hexKey)` loads a key. `exchange.New(url, signer)` trades for the signer's own account; `NewForVault(url, signer, vault)` for a vault; `SetPrincipal(master)` records the master an API wallet acts for. Call `VerifySubAccount` before trading for a vault.
- Set `SignChainID` through `Client.LoadChainID`; there is no default, and a wrong value gives code 91.
- `Client.Exchange(ctx, action)` signs, sends and reconciles. `LastOrderIDs`, `LastStatuses` and `LastReceipt` describe the last request. `RecoverPending(ctx)` only observes: it queries and never submits.
- Set `Client.WS` and `Client.Queries` to a `*binws.Session` to use bin-ws for actions and reconciliation queries; leave them nil for REST.
- `binws.Config.OnFrame` receives pushes; `Router.Handle` is a ready callback. `Config.Sign` takes an `exchange.Signer`.
- Errors: `errors.As` into `*exchange.UnknownOutcomeError` to get the nonce, account, signed action and cloids to reconcile; `errors.Is(err, exchange.ErrExecutionRejected)` for a final rejection.

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
```

| Variable | Default | Meaning |
|---|---|---|
| `BIN_WS_URL` | `wss://pre-api.riverbit.io/bin-ws` | bin-ws endpoint |
| `BIN_WS_LOGIN_HOST` | `pre-api.riverbit.io` | `host:` line of the login message; must equal the server's public host |
| `API_GATEWAY_URL` | `https://pre-api.riverbit.io` | REST base URL |
| `COIN` | `BTC` | market to watch and trade |
| `BOOK_STREAM` | `l2` | `l2` (incremental) or `l2Book` (snapshots) |
| `ORDER_TRANSPORT` | `ws` | `ws` (one bin-ws connection) or `rest` |
| `GUILD_VAULT_ADDRESS` | empty | trade for this vault; the signer must lead it or be its API wallet |
| `KEY_FILE` | `key` | file with the private key |
| `STATE_DIR` | `state` | durable pending journal directory |
| `SIGN_CHAIN_ID` | empty | if set, must equal `chainIdentity` |
| `ACTION_FILE` | none | action JSON files for `trade` |

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
