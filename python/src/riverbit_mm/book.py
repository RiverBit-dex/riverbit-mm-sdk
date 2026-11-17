"""Local order book that replays an ``l2`` subscription.

Rules (guide section 5):

1. A snapshot replaces the whole book; its decimals become the reference.
2. An update lists removed levels as indices into the **previous** book and they
   are removed as one set.
3. Changed levels overwrite by price, or are inserted.
4. Bids are re-sorted descending, asks ascending.

An update whose decimals differ from the latest snapshot, that arrives before
any snapshot, or that would leave the book unordered raises :class:`BookError`
and leaves the book untouched. The caller should then reset the book and
unsubscribe / resubscribe the stream to get a fresh snapshot.

Prices and sizes are kept as :class:`decimal.Decimal` for arithmetic and as the
original text for display and for use in orders.
"""

from __future__ import annotations

from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
from typing import Any

from .errors import ProtocolError
from .frames import JsonFrame, L2Book, L2Delta, L2DeltaUpdate, format_fixed


class BookError(ProtocolError):
    """An ``l2`` frame cannot be applied; the book needs a fresh snapshot."""


@dataclass(frozen=True)
class Level:
    px: Decimal
    sz: Decimal
    px_text: str
    sz_text: str


def _level(px: str, sz: str) -> Level:
    try:
        return Level(Decimal(px), Decimal(sz), px, sz)
    except InvalidOperation as exc:
        raise BookError(f"invalid l2 level px={px!r} sz={sz!r}") from exc


def _fixed_levels(px: tuple[int, ...], sz: tuple[int, ...], pxd: int, szd: int) -> list[Level]:
    return [_level(format_fixed(p, pxd), format_fixed(s, szd)) for p, s in zip(px, sz, strict=True)]


def _check_ordered(bids: list[Level], asks: list[Level]) -> None:
    for name, levels, descending in (("bid", bids, True), ("ask", asks, False)):
        for i in range(1, len(levels)):
            prev, cur = levels[i - 1].px, levels[i].px
            if (descending and prev <= cur) or (not descending and prev >= cur):
                raise BookError(
                    f"{name} side is not strictly ordered at level {i} "
                    f"({levels[i - 1].px_text}, {levels[i].px_text})"
                )


class Book:
    """The book of one coin, fed by ``l2`` frames (typed or JSON fallback)."""

    def __init__(self, coin: str = "") -> None:
        self.coin = coin
        self.reset()

    def reset(self) -> None:
        """Forget the book (call after a reconnect or on :class:`BookError`)."""
        self.bids: list[Level] = []
        self.asks: list[Level] = []
        self.px_decimals = 0
        self.sz_decimals = 0
        #: True after a typed snapshot: typed updates must carry the same decimals.
        self.locked = False
        self.has_book = False
        self.time = 0
        self.last_spread: Decimal | None = None

    def best(self) -> tuple[Level | None, Level | None]:
        """Best bid and best ask; ``None`` for an empty side."""
        return (self.bids[0] if self.bids else None, self.asks[0] if self.asks else None)

    # ------------------------------------------------------------ dispatch

    def apply(self, body: Any) -> bool:
        """Apply an ``l2`` frame body.

        Accepts a typed :class:`~riverbit_mm.frames.L2Delta` and a
        :class:`~riverbit_mm.frames.JsonFrame` of channel ``l2`` (the fallback
        form). Returns ``False`` for any other frame, ``True`` once applied.
        """
        if isinstance(body, L2Delta):
            if body.snapshot is not None:
                self._apply_snapshot(body.snapshot)
            elif body.update is not None:
                self._apply_update(body.update)
            else:
                raise BookError("l2 delta without snapshot or update")
            return True
        if isinstance(body, JsonFrame):
            if body.channel != "l2":
                return False
            self.apply_json(body.data)
            return True
        return False

    # ------------------------------------------------------------ typed

    def _apply_snapshot(self, snap: L2Book) -> None:
        if self.coin and snap.coin != self.coin:
            raise BookError(f"l2 snapshot for {snap.coin!r}, subscribed {self.coin!r}")
        bids = _fixed_levels(snap.bid_px, snap.bid_sz, snap.px_decimals, snap.sz_decimals)
        asks = _fixed_levels(snap.ask_px, snap.ask_sz, snap.px_decimals, snap.sz_decimals)
        _check_ordered(bids, asks)
        self.bids, self.asks = bids, asks
        self.px_decimals, self.sz_decimals = snap.px_decimals, snap.sz_decimals
        self.locked = self.has_book = True
        self.time = snap.time
        self.last_spread = None

    def _apply_update(self, upd: L2DeltaUpdate) -> None:
        if not self.has_book:
            raise BookError("l2 update before any snapshot")
        if self.coin and upd.coin != self.coin:
            raise BookError(f"l2 update for {upd.coin!r}, subscribed {self.coin!r}")
        if not self.locked:
            raise BookError("typed l2 update on a book built from a JSON snapshot")
        if (upd.px_decimals, upd.sz_decimals) != (self.px_decimals, self.sz_decimals):
            raise BookError(
                f"l2 update decimals ({upd.px_decimals},{upd.sz_decimals}) differ from "
                f"the snapshot's ({self.px_decimals},{self.sz_decimals})"
            )
        changed = (
            _fixed_levels(upd.bid_px, upd.bid_sz, upd.px_decimals, upd.sz_decimals),
            _fixed_levels(upd.ask_px, upd.ask_sz, upd.px_decimals, upd.sz_decimals),
        )
        self._replay(changed, (upd.bid_removed, upd.ask_removed))
        self.time = upd.time
        self.last_spread = (
            None if upd.spread is None else Decimal(format_fixed(upd.spread, upd.px_decimals))
        )

    def _replay(
        self,
        changed: tuple[list[Level], list[Level]],
        removed: tuple[tuple[int, ...], tuple[int, ...]],
    ) -> None:
        # Compute both sides first and commit only if the result is valid.
        result: list[list[Level]] = []
        for side, previous in enumerate((self.bids, self.asks)):
            drop = set(removed[side])
            for index in drop:
                if index >= len(previous):
                    raise BookError(
                        f"removed index {index} outside the previous book ({len(previous)} levels)"
                    )
            by_price = {lvl.px: lvl for i, lvl in enumerate(previous) if i not in drop}
            for lvl in changed[side]:
                by_price[lvl.px] = lvl
            result.append(sorted(by_price.values(), key=lambda lvl: lvl.px, reverse=side == 0))
        _check_ordered(result[0], result[1])
        self.bids, self.asks = result

    # ------------------------------------------------------------ JSON fallback

    def apply_json(self, data: Any) -> None:
        """Apply the ``data`` member of a ``{"channel":"l2"}`` JSON frame.

        Either ``{"s": {"coin","time","levels":[[{px,sz}..],[..]]}}`` (snapshot)
        or ``{"u": {"c","t","l":[[{p,s}..],[..]],"r":[[idx..],[idx..]],"s":spread}}``.
        """
        if not isinstance(data, dict):
            raise BookError("l2 JSON frame is not an object")
        snap, upd = data.get("s"), data.get("u")
        try:
            if isinstance(snap, dict):
                self._apply_json_snapshot(snap)
            elif isinstance(upd, dict):
                self._apply_json_update(upd)
            else:
                raise BookError("l2 JSON frame without s or u")
        except (KeyError, TypeError, ValueError) as exc:
            raise BookError(f"malformed l2 JSON frame: {exc!r}") from exc

    def _apply_json_snapshot(self, snap: dict[str, Any]) -> None:
        coin = snap.get("coin", "")
        if self.coin and coin and coin != self.coin:
            raise BookError(f"l2 JSON snapshot for {coin!r}, subscribed {self.coin!r}")
        levels = snap["levels"]
        bids = [_level(x["px"], x["sz"]) for x in levels[0]]
        asks = [_level(x["px"], x["sz"]) for x in levels[1]]
        _check_ordered(bids, asks)
        self.bids, self.asks = bids, asks
        self.locked, self.has_book = False, True
        self.time = int(snap.get("time", 0))
        self.last_spread = None

    def _apply_json_update(self, upd: dict[str, Any]) -> None:
        if not self.has_book:
            raise BookError("l2 JSON update before any snapshot")
        coin = upd.get("c", "")
        if self.coin and coin and coin != self.coin:
            raise BookError(f"l2 JSON update for {coin!r}, subscribed {self.coin!r}")
        lv = upd.get("l", [[], []])
        changed = (
            [_level(x["p"], x["s"]) for x in lv[0]],
            [_level(x["p"], x["s"]) for x in lv[1]],
        )
        rm = upd.get("r") or [[], []]
        self._replay(changed, (tuple(int(i) for i in rm[0]), tuple(int(i) for i in rm[1])))
        self.time = int(upd.get("t", 0))
        spread = upd.get("s")
        self.last_spread = None
        if spread not in (None, ""):
            try:
                self.last_spread = Decimal(str(spread))
            except InvalidOperation:
                self.last_spread = None
