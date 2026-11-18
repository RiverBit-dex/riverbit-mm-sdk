# Changelog

Notable changes to this repository. Each language library is versioned on its own; entries name the language they affect.

## Unreleased

- Protocol guide, bin-ws Protobuf definitions, golden signing vectors and bin-ws frame fixtures shared by all three libraries.
- Go: packages `exchange` and `binws`, the `mmcli` example, and a durable pending-request journal with recovery. Requires Go 1.24 or newer.
- Python: package `riverbit_mm` (asyncio, Python 3.10 or newer) with examples.
- Rust: crate `riverbit-mm-sdk` (tokio, rustls) with examples.
- Orders, cancellations and modifications are submitted over REST `/exchange` only; bin-ws carries market data, order and fill events and `info_json` queries. Field 2 of `WsV2ClientFrame` is reserved.
- Request quota: `userRateLimit` query, `reserveRequestWeight` purchases, recognition of quota rejections (codes 1800 and 1801), and three more signing vectors. The guide's section 10.1 describes the rules.
- Optional gateway credential sent as `X-RiverBit-MM-Token` on REST requests.
- All libraries treat duplicate-nonce replies, incomplete receipts and redirects as unknown outcomes, and never resend a signed request on their own.
