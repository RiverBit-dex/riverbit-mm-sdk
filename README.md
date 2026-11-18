# RiverBit Market Maker SDK

Client libraries for market makers integrating with the RiverBit exchange API: market data, order entry, cancellation, modification and reconciliation over REST and the binary WebSocket endpoint (`bin-ws`).

> **Status: pre-release.** Interfaces may change before `v0.1.0`. The SDK contains no quoting strategy; it gives you the protocol, signing and reconciliation building blocks to run your own.

## Languages

| Language | Directory | Durable pending-request journal |
|---|---|---|
| Go | [`go/`](go/) | Yes |
| Python | [`python/`](python/) | Not yet |
| Rust | [`rust/`](rust/) | Not yet |

Each library covers signing, order entry, cancellation, modification, REST queries, bin-ws market data and order events, a local order book and reconciliation helpers. The Go library also persists pending requests to disk and recovers them after a restart; with Python and Rust, persist the signed request yourself before sending it.

All three implementations follow the same protocol guide, share one Protobuf definition and must reproduce the same golden signing vectors byte for byte.

## Repository layout

| Path | Contents |
|---|---|
| [`docs/api-guide.md`](docs/api-guide.md) | Protocol guide: endpoints, signing, bin-ws frames, orders, reconciliation, errors and limits |
| [`proto/binws.proto`](proto/binws.proto) | Protobuf definitions for bin-ws frames |
| [`testdata/vectors.json`](testdata/vectors.json) | Golden vectors for login and trading signatures |
| `go/`, `python/`, `rust/` | One self-contained library per language, each with its own README, tests and examples |

## Endpoints

| Purpose | Endpoint |
|---|---|
| REST queries | `https://pre-api.riverbit.io/info` |
| REST trading | `https://pre-api.riverbit.io/exchange` |
| WebSocket (market data, order events, queries) | `wss://pre-api.riverbit.io/bin-ws` |

Orders, cancellations and modifications go through REST `/exchange`; the public WebSocket endpoint carries market data, order and fill events and queries, and does not accept order entry.

Every client takes the base URL as configuration; nothing is tied to one environment.

## Before you trade

- An API wallet signs requests; the master account (or a vault it leads) is the trading account. The master account must authorize the API wallet first.
- Keep private keys out of source control, command-line arguments, logs and action files. The libraries never print key material.
- **Never re-sign an action whose outcome is unknown with a new nonce.** Reconcile it first. The [protocol guide](docs/api-guide.md) explains why and how.
- Query asset metadata at startup instead of hard-coding asset indexes or precision.

## License

[Apache-2.0](LICENSE)
