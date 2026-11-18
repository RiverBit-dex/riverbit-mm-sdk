# Changelog

Notable changes to this repository. Each language library is versioned on its own; entries name the language they affect.

## Unreleased

- Protocol guide, bin-ws Protobuf definitions, golden signing vectors and bin-ws frame fixtures shared by all three libraries.
- Go: packages `exchange` and `binws`, the `mmcli` example, and a durable pending-request journal with recovery. Requires Go 1.24 or newer.
- Python: package `riverbit_mm` (asyncio, Python 3.10 or newer) with examples.
- Rust: crate `riverbit-mm-sdk` (tokio, rustls) with examples.
- All libraries send orders over REST `/exchange`, treat duplicate-nonce replies, incomplete receipts and redirects as unknown outcomes, and never resend a signed request on their own.
