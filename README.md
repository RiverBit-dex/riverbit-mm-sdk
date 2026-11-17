# RiverBit Market Maker SDK

Client libraries for market makers integrating with the RiverBit exchange API: market data, order entry, cancellation, modification and reconciliation over REST and the binary WebSocket endpoint (`bin-ws`).

> **Status: pre-release.** Interfaces may change before `v0.1.0`. The SDK contains no quoting strategy; it gives you the protocol, signing and reconciliation building blocks to run your own.

## Languages

| Language | Directory | Status |
|---|---|---|
| Go | [`go/`](go/) | In development |
| Python | [`python/`](python/) | In development |
| Rust | [`rust/`](rust/) | In development |

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
| WebSocket | `wss://pre-api.riverbit.io/bin-ws` |

Every client takes the base URL as configuration; nothing is tied to one environment.

## Before you trade

- An API wallet signs requests; the master account (or a vault it leads) is the trading account. The master account must authorize the API wallet first.
- Keep private keys out of source control, command-line arguments, logs and action files. The libraries never print key material.
- **Never re-sign an action whose outcome is unknown with a new nonce.** Reconcile it first. The [protocol guide](docs/api-guide.md) explains why and how.
- Query asset metadata at startup instead of hard-coding asset indexes or precision.

## License

[Apache-2.0](LICENSE)
