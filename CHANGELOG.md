# Changelog

Notable changes to this repository. Each language library is versioned on its own; entries name the language they affect.

## Unreleased

- Repository layout, protocol guide, bin-ws Protobuf definitions and golden signing vectors.
- Go: minimum Go version lowered to 1.24 (the lowest the dependencies allow); `/info` reads wait for `Retry-After` on 429 and otherwise use capped exponential backoff (a 429 on a signed action stays an unknown outcome); action type naming (`Place`, `Order`, `CancelItem`, `ModifyItem`) documented.
- Python: README documents the action type naming (`Place`, `Order`, `CancelItem`, `ModifyItem`) and the shared pre-release wording and repository URL.
- Rust: README documents the action type naming (`Place`, `Order`, `CancelItem`, `ModifyItem`) and the shared pre-release wording and repository URL.
- Protocol guide: `/info` table lists `subAccounts`, `vaultRegistry` and `vaultDetails`. Test data README notes that fixture bytes follow the Go encoder's field order and decoders must not depend on it.
