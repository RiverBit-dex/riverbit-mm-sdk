"""Shared configuration for the examples.

Settings come from environment variables. The defaults below are the PRE
environment; override them to point at another environment.

    RIVERBIT_KEY_FILE      path of a file holding the API wallet private key (one line)
    RIVERBIT_BASE_URL      REST base URL        (default https://pre-api.riverbit.io)
    RIVERBIT_WS_URL        bin-ws URL           (default wss://pre-api.riverbit.io/bin-ws)
    RIVERBIT_LOGIN_HOST    login host           (default pre-api.riverbit.io)
    RIVERBIT_VAULT         vault address to trade for (optional)
    RIVERBIT_MM_TOKEN      gateway credential issued to you (optional); sent as the
                           X-RiverBit-MM-Token header on REST requests only

The private key is read from a file, never from the command line or the
environment, so it does not end up in shell history or process listings.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

from riverbit_mm import Signer

PRE_BASE_URL = "https://pre-api.riverbit.io"
PRE_WS_URL = "wss://pre-api.riverbit.io/bin-ws"
PRE_LOGIN_HOST = "pre-api.riverbit.io"


@dataclass(frozen=True)
class Config:
    base_url: str
    ws_url: str
    login_host: str
    signer: Signer
    vault: str | None
    gateway_token: str | None = None


def load_config() -> Config:
    key_file = os.environ.get("RIVERBIT_KEY_FILE")
    if not key_file:
        raise SystemExit("set RIVERBIT_KEY_FILE to a file containing the API wallet private key")
    key = Path(key_file).read_text().strip()
    return Config(
        base_url=os.environ.get("RIVERBIT_BASE_URL", PRE_BASE_URL),
        ws_url=os.environ.get("RIVERBIT_WS_URL", PRE_WS_URL),
        login_host=os.environ.get("RIVERBIT_LOGIN_HOST", PRE_LOGIN_HOST),
        signer=Signer(key),
        vault=os.environ.get("RIVERBIT_VAULT") or None,
        gateway_token=os.environ.get("RIVERBIT_MM_TOKEN") or None,
    )
