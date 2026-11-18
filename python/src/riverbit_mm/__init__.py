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
    ReserveRequestWeight,
    reserve_cost_micro_usdc,
)
from .book import Book, BookError, Level
from .errors import (
    QUOTA_ERROR_TEXT,
    QUOTA_EXHAUSTED_CODE,
    RESERVE_OVERFLOW_CODE,
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
    is_quota_error,
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
from .info import Asset, InfoClient, UserRateLimit, parse_meta, parse_user_rate_limit
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
    "QUOTA_ERROR_TEXT",
    "QUOTA_EXHAUSTED_CODE",
    "RESERVE_OVERFLOW_CODE",
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
    "ReserveRequestWeight",
    "RiverBitError",
    "ServerFrame",
    "SessionClosed",
    "Signature",
    "SignedRequest",
    "Signer",
    "Trade",
    "Trades",
    "UnknownOutcome",
    "UserRateLimit",
    "format_fixed",
    "is_quota_error",
    "order_state",
    "parse_meta",
    "parse_user_rate_limit",
    "reconcile_unknown",
    "reserve_cost_micro_usdc",
    "shared_nonce_source",
    "subscriptions",
    "wait_reconciled",
]
