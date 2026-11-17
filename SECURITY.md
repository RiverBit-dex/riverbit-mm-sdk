# Security

## Reporting a vulnerability

Please report security issues privately through GitHub's "Report a vulnerability" form under this repository's **Security** tab. Do not open a public issue for a suspected vulnerability.

Include the affected language and version, a description of the problem and, where possible, steps to reproduce it.

## Handling keys

- Never commit a private key, and never paste one into an issue, pull request or log.
- Load keys from a file or a secret manager, not from command-line arguments.
- The key in `testdata/vectors.json` is a published test key for offline signature checks. Never send assets to its address.
