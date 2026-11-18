"""RiverBit market maker SDK.

Signing, actions, REST clients, the bin-ws session, typed frames, a local book
and reconciliation helpers for market makers integrating with the RiverBit
exchange API. The SDK contains no quoting strategy.
"""

from __future__ import annotations

from . import subscriptions
from .actions import (
    Action,
    BatchModify,
    Cancel,
    CancelByCloid,
    CancelByCloidItem,
    CancelItem,
    Cloid,
    Modify,
    ModifyItem,
    Order,
    Place,
)
from .book import Book, BookError, Level
from .errors import (
    ActionRejected,
    HttpError,
    LoginFailed,
    NotConnected,
    PostError,
    ProtocolError,
    RateLimited,
    RequestTimeout,
    RiverBitError,
    SessionClosed,
    UnknownOutcome,
)
from .exchange import ExchangeClient
from .frames import (
    Bbo,
    Candle,
    JsonFrame,
    L2Book,
    L2Delta,
    L2DeltaUpdate,
    LoginResult,
    ServerFrame,
    Trade,
    Trades,
    format_fixed,
)
from .info import Asset, InfoClient, parse_meta
from .nonce import NonceSource, shared_nonce_source
from .receipts import Leg, Receipt
from .reconcile import (
    OrderState,
    Reconciliation,
    order_state,
    reconcile_unknown,
    wait_reconciled,
)
from .signing import Signature, SignedRequest, Signer
from .ws import BinWsSession, Connected, Disconnected

__version__ = "0.1.0.dev0"

__all__ = [
    "Action",
    "ActionRejected",
    "Asset",
    "BatchModify",
    "Bbo",
    "BinWsSession",
    "Book",
    "BookError",
    "Cancel",
    "CancelByCloid",
    "CancelByCloidItem",
    "CancelItem",
    "Candle",
    "Cloid",
    "Connected",
    "Disconnected",
    "ExchangeClient",
    "HttpError",
    "InfoClient",
    "JsonFrame",
    "L2Book",
    "L2Delta",
    "L2DeltaUpdate",
    "Leg",
    "Level",
    "LoginFailed",
    "LoginResult",
    "Modify",
    "ModifyItem",
    "NonceSource",
    "NotConnected",
    "Order",
    "OrderState",
    "Place",
    "PostError",
    "ProtocolError",
    "RateLimited",
    "Receipt",
    "Reconciliation",
    "RequestTimeout",
    "RiverBitError",
    "ServerFrame",
    "SessionClosed",
    "Signature",
    "SignedRequest",
    "Signer",
    "Trade",
    "Trades",
    "UnknownOutcome",
    "format_fixed",
    "order_state",
    "parse_meta",
    "reconcile_unknown",
    "shared_nonce_source",
    "subscriptions",
    "wait_reconciled",
]
