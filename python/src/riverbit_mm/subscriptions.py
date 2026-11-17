"""Builders for ``subscribe_json`` objects (the subscription itself, no ``method`` wrapper)."""

from __future__ import annotations

from typing import Any

#: Candle intervals the endpoint accepts.
CANDLE_INTERVALS = (
    "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1d", "3d", "1w",
)  # fmt: skip

Subscription = dict[str, Any]


def l2(coin: str, n_sig_figs: int | None = None) -> Subscription:
    """Incremental book (typed ``l2`` frames). A RiverBit extension: uses ``c``, not ``coin``."""
    sub: Subscription = {"type": "l2", "c": coin}
    if n_sig_figs is not None:
        sub["s"] = n_sig_figs
    return sub


def l2_book(coin: str, n_sig_figs: int | None = None, mantissa: int | None = None) -> Subscription:
    """Full book snapshots (typed ``l2_book`` frames)."""
    sub: Subscription = {"type": "l2Book", "coin": coin}
    if n_sig_figs is not None:
        sub["nSigFigs"] = n_sig_figs
    if mantissa is not None:
        sub["mantissa"] = mantissa
    return sub


def bbo(coin: str) -> Subscription:
    return {"type": "bbo", "coin": coin}


def trades(coin: str) -> Subscription:
    return {"type": "trades", "coin": coin}


def candle(coin: str, interval: str) -> Subscription:
    if interval not in CANDLE_INTERVALS:
        raise ValueError(f"interval must be one of {CANDLE_INTERVALS}")
    return {"type": "candle", "coin": coin, "interval": interval}


def order_updates(user: str) -> Subscription:
    """Order events of a master or vault. Contains changes only; fetch ``openOrders`` separately."""
    return {"type": "orderUpdates", "user": user}


def user_fills(user: str) -> Subscription:
    return {"type": "userFills", "user": user}


def open_orders(user: str) -> Subscription:
    return {"type": "openOrders", "user": user}


def clearinghouse_state(user: str) -> Subscription:
    return {"type": "clearinghouseState", "user": user}
