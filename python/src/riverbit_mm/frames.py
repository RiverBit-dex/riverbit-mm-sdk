"""bin-ws frames: typed messages with their Protobuf encoders and decoders.

The shapes mirror ``proto/binws.proto``. Fixed-point fields hold the raw
integers; divide by ``10 ** decimals`` (or call :func:`format_fixed`) to get the
decimal value. ``encode()`` methods exist on every type so tests and mock servers
can produce the same bytes the real endpoint sends.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

from . import _proto as pb
from .errors import ProtocolError

# oneof field numbers of WsV2ClientFrame
CLIENT_ACTION = 2
CLIENT_INFO = 3
CLIENT_SUBSCRIBE = 4
CLIENT_UNSUBSCRIBE = 5
CLIENT_PING = 6
CLIENT_LOGIN = 7

# oneof field numbers of WsV2ServerFrame
KIND_JSON = 2
KIND_L2_BOOK = 3
KIND_L2_DELTA = 4
KIND_BBO = 5
KIND_CANDLE = 6
KIND_TRADES = 7
KIND_LOGIN = 8


def format_fixed(value: int, decimals: int) -> str:
    """Render ``value / 10**decimals`` exactly, e.g. ``format_fixed(1010, 1) == "101.0"``."""
    text = str(value)
    if decimals == 0:
        return text
    if len(text) <= decimals:
        text = "0" * (decimals - len(text) + 1) + text
    return text[:-decimals] + "." + text[-decimals:]


# ---------------------------------------------------------------- client frames


@dataclass(frozen=True)
class ClientFrame:
    """A decoded ``WsV2ClientFrame``. ``body`` is the oneof field number."""

    id: int
    body: int
    data: bytes

    def encode(self) -> bytes:
        return pb.enc_varint(1, self.id) + pb.enc_bytes_always(self.body, self.data)


def decode_client_frame(data: bytes) -> ClientFrame:
    msg = pb.Message(data)
    found = [n for n in range(CLIENT_ACTION, CLIENT_LOGIN + 1) if msg.has(n)]
    if not found:
        raise ProtocolError("client frame has no body")
    if len(found) > 1:
        raise ProtocolError("client frame has more than one body")
    return ClientFrame(msg.uint(1), found[0], msg.bytes_(found[0]))


@dataclass(frozen=True)
class Login:
    timestamp_ms: int
    nonce: bytes
    signature: bytes

    def encode(self) -> bytes:
        return (
            pb.enc_varint(1, self.timestamp_ms)
            + pb.enc_bytes(2, self.nonce)
            + pb.enc_bytes(3, self.signature)
        )

    @classmethod
    def decode(cls, data: bytes) -> Login:
        msg = pb.Message(data)
        return cls(msg.uint(1), msg.bytes_(2), msg.bytes_(3))


@dataclass(frozen=True)
class LoginResult:
    ok: bool
    user: bytes = b""
    signer: bytes = b""
    error: str = ""

    def encode(self) -> bytes:
        return (
            pb.enc_bool(1, self.ok)
            + pb.enc_bytes(2, self.user)
            + pb.enc_bytes(3, self.signer)
            + pb.enc_string(4, self.error)
        )

    @classmethod
    def decode(cls, data: bytes) -> LoginResult:
        msg = pb.Message(data)
        return cls(msg.boolean(1), msg.bytes_(2), msg.bytes_(3), msg.string(4))

    @property
    def user_address(self) -> str:
        """Resolved master account, lowercase ``0x`` form ('' if absent)."""
        return "0x" + self.user.hex() if self.user else ""

    @property
    def signer_address(self) -> str:
        """Recovered signer: the API wallet or the master itself ('' if absent)."""
        return "0x" + self.signer.hex() if self.signer else ""


# ---------------------------------------------------------------- market data


def _check_lengths(name: str, *pairs: tuple[tuple[int, ...], tuple[int, ...]]) -> None:
    for a, b in pairs:
        if len(a) != len(b):
            raise ProtocolError(f"{name}: parallel arrays differ in length")


@dataclass(frozen=True)
class L2Book:
    """Full book snapshot (channel ``l2Book``, or the snapshot of an ``l2`` stream)."""

    coin: str
    time: int
    px_decimals: int
    sz_decimals: int
    bid_px: tuple[int, ...] = ()
    bid_sz: tuple[int, ...] = ()
    bid_n: tuple[int, ...] = ()
    ask_px: tuple[int, ...] = ()
    ask_sz: tuple[int, ...] = ()
    ask_n: tuple[int, ...] = ()

    def encode(self) -> bytes:
        return (
            pb.enc_string(1, self.coin)
            + pb.enc_varint(2, self.time)
            + pb.enc_varint(3, self.px_decimals)
            + pb.enc_varint(4, self.sz_decimals)
            + pb.enc_packed(5, self.bid_px)
            + pb.enc_packed(6, self.bid_sz)
            + pb.enc_packed(7, self.bid_n)
            + pb.enc_packed(8, self.ask_px)
            + pb.enc_packed(9, self.ask_sz)
            + pb.enc_packed(10, self.ask_n)
        )

    @classmethod
    def decode(cls, data: bytes) -> L2Book:
        m = pb.Message(data)
        book = cls(
            coin=m.string(1),
            time=m.uint(2),
            px_decimals=m.uint(3, 32),
            sz_decimals=m.uint(4, 32),
            bid_px=tuple(m.repeated(5)),
            bid_sz=tuple(m.repeated(6)),
            bid_n=tuple(m.repeated(7, 32)),
            ask_px=tuple(m.repeated(8)),
            ask_sz=tuple(m.repeated(9)),
            ask_n=tuple(m.repeated(10, 32)),
        )
        _check_lengths(
            f"l2 book {book.coin}",
            (book.bid_px, book.bid_sz),
            (book.ask_px, book.ask_sz),
        )
        for n, px in ((book.bid_n, book.bid_px), (book.ask_n, book.ask_px)):
            if n and len(n) != len(px):
                raise ProtocolError(f"l2 book {book.coin}: n array differs in length")
        return book


@dataclass(frozen=True)
class L2DeltaUpdate:
    """Update against the previous book of an ``l2`` stream."""

    coin: str
    time: int
    px_decimals: int
    sz_decimals: int
    bid_px: tuple[int, ...] = ()
    bid_sz: tuple[int, ...] = ()
    ask_px: tuple[int, ...] = ()
    ask_sz: tuple[int, ...] = ()
    bid_removed: tuple[int, ...] = ()
    ask_removed: tuple[int, ...] = ()
    spread: int | None = None

    def encode(self) -> bytes:
        out = (
            pb.enc_string(1, self.coin)
            + pb.enc_varint(2, self.time)
            + pb.enc_varint(3, self.px_decimals)
            + pb.enc_varint(4, self.sz_decimals)
            + pb.enc_packed(5, self.bid_px)
            + pb.enc_packed(6, self.bid_sz)
            + pb.enc_packed(7, self.ask_px)
            + pb.enc_packed(8, self.ask_sz)
            + pb.enc_packed(9, self.bid_removed)
            + pb.enc_packed(10, self.ask_removed)
        )
        if self.spread is not None:
            out += pb.enc_varint_always(11, self.spread)
        return out

    @classmethod
    def decode(cls, data: bytes) -> L2DeltaUpdate:
        m = pb.Message(data)
        update = cls(
            coin=m.string(1),
            time=m.uint(2),
            px_decimals=m.uint(3, 32),
            sz_decimals=m.uint(4, 32),
            bid_px=tuple(m.repeated(5)),
            bid_sz=tuple(m.repeated(6)),
            ask_px=tuple(m.repeated(7)),
            ask_sz=tuple(m.repeated(8)),
            bid_removed=tuple(m.repeated(9, 32)),
            ask_removed=tuple(m.repeated(10, 32)),
            spread=m.opt_uint(11),
        )
        _check_lengths(
            f"l2 update {update.coin}",
            (update.bid_px, update.bid_sz),
            (update.ask_px, update.ask_sz),
        )
        return update


@dataclass(frozen=True)
class L2Delta:
    """Typed ``l2`` frame: exactly one of ``snapshot`` and ``update`` is set."""

    snapshot: L2Book | None = None
    update: L2DeltaUpdate | None = None

    def encode(self) -> bytes:
        if self.snapshot is not None:
            return pb.enc_bytes_always(1, self.snapshot.encode())
        if self.update is not None:
            return pb.enc_bytes_always(2, self.update.encode())
        return b""

    @classmethod
    def decode(cls, data: bytes) -> L2Delta:
        m = pb.Message(data)
        if m.has(1) and m.has(2):
            raise ProtocolError("l2 delta has both snapshot and update")
        if m.has(1):
            return cls(snapshot=L2Book.decode(m.bytes_(1)))
        if m.has(2):
            return cls(update=L2DeltaUpdate.decode(m.bytes_(2)))
        raise ProtocolError("l2 delta without snapshot or update")


@dataclass(frozen=True)
class Bbo:
    """Best bid and offer. A side without a level has all of its fields ``None``."""

    coin: str
    time: int
    px_decimals: int
    sz_decimals: int
    bid_px: int | None = None
    bid_sz: int | None = None
    bid_n: int | None = None
    ask_px: int | None = None
    ask_sz: int | None = None
    ask_n: int | None = None

    def encode(self) -> bytes:
        out = (
            pb.enc_string(1, self.coin)
            + pb.enc_varint(2, self.time)
            + pb.enc_varint(3, self.px_decimals)
            + pb.enc_varint(4, self.sz_decimals)
        )
        # Field order matches the golden frames (counts last), not field-number order.
        for number, value in (
            (5, self.bid_px),
            (6, self.bid_sz),
            (8, self.ask_px),
            (9, self.ask_sz),
            (7, self.bid_n),
            (10, self.ask_n),
        ):
            if value is not None:
                out += pb.enc_varint_always(number, value)
        return out

    @classmethod
    def decode(cls, data: bytes) -> Bbo:
        m = pb.Message(data)
        return cls(
            coin=m.string(1),
            time=m.uint(2),
            px_decimals=m.uint(3, 32),
            sz_decimals=m.uint(4, 32),
            bid_px=m.opt_uint(5),
            bid_sz=m.opt_uint(6),
            bid_n=m.opt_uint(7, 32),
            ask_px=m.opt_uint(8),
            ask_sz=m.opt_uint(9),
            ask_n=m.opt_uint(10, 32),
        )

    @property
    def bid(self) -> tuple[str, str] | None:
        """``(price, size)`` of the best bid as decimal strings, if present."""
        if self.bid_px is None or self.bid_sz is None:
            return None
        return format_fixed(self.bid_px, self.px_decimals), format_fixed(
            self.bid_sz, self.sz_decimals
        )

    @property
    def ask(self) -> tuple[str, str] | None:
        """``(price, size)`` of the best ask as decimal strings, if present."""
        if self.ask_px is None or self.ask_sz is None:
            return None
        return format_fixed(self.ask_px, self.px_decimals), format_fixed(
            self.ask_sz, self.sz_decimals
        )


@dataclass(frozen=True)
class Candle:
    coin: str
    interval: str
    open_time: int
    close_time: int
    px_decimals: int
    sz_decimals: int
    open: int
    close: int
    high: int
    low: int
    volume: int  # scaled by sz_decimals
    trade_count: int

    def encode(self) -> bytes:
        return (
            pb.enc_string(1, self.coin)
            + pb.enc_string(2, self.interval)
            + pb.enc_varint(3, self.open_time)
            + pb.enc_varint(4, self.close_time)
            + pb.enc_varint(5, self.px_decimals)
            + pb.enc_varint(6, self.sz_decimals)
            + pb.enc_varint(7, self.open)
            + pb.enc_varint(8, self.close)
            + pb.enc_varint(9, self.high)
            + pb.enc_varint(10, self.low)
            + pb.enc_varint(11, self.volume)
            + pb.enc_varint(12, self.trade_count)
        )

    @classmethod
    def decode(cls, data: bytes) -> Candle:
        m = pb.Message(data)
        return cls(
            coin=m.string(1),
            interval=m.string(2),
            open_time=m.uint(3),
            close_time=m.uint(4),
            px_decimals=m.uint(5, 32),
            sz_decimals=m.uint(6, 32),
            open=m.uint(7),
            close=m.uint(8),
            high=m.uint(9),
            low=m.uint(10),
            volume=m.uint(11),
            trade_count=m.uint(12, 32),
        )

    def px(self, value: int) -> str:
        return format_fixed(value, self.px_decimals)

    @property
    def volume_text(self) -> str:
        return format_fixed(self.volume, self.sz_decimals)


@dataclass(frozen=True)
class Trade:
    buy: bool  # the aggressor bought
    px: int
    sz: int
    time: int
    tid: int
    hash: bytes = b""
    buyer: bytes = b""  # 20 bytes
    seller: bytes = b""  # 20 bytes

    def encode(self) -> bytes:
        return (
            pb.enc_bool(1, self.buy)
            + pb.enc_varint(2, self.px)
            + pb.enc_varint(3, self.sz)
            + pb.enc_varint(4, self.time)
            + pb.enc_varint(5, self.tid)
            + pb.enc_bytes(6, self.hash)
            + pb.enc_bytes(7, self.buyer)
            + pb.enc_bytes(8, self.seller)
        )

    @classmethod
    def decode(cls, data: bytes) -> Trade:
        m = pb.Message(data)
        return cls(
            buy=m.boolean(1),
            px=m.uint(2),
            sz=m.uint(3),
            time=m.uint(4),
            tid=m.uint(5),
            hash=m.bytes_(6),
            buyer=m.bytes_(7),
            seller=m.bytes_(8),
        )


@dataclass(frozen=True)
class Trades:
    coin: str
    px_decimals: int
    sz_decimals: int
    trades: tuple[Trade, ...] = ()

    def encode(self) -> bytes:
        out = (
            pb.enc_string(1, self.coin)
            + pb.enc_varint(2, self.px_decimals)
            + pb.enc_varint(3, self.sz_decimals)
        )
        for trade in self.trades:
            out += pb.enc_bytes_always(4, trade.encode())
        return out

    @classmethod
    def decode(cls, data: bytes) -> Trades:
        m = pb.Message(data)
        return cls(
            coin=m.string(1),
            px_decimals=m.uint(2, 32),
            sz_decimals=m.uint(3, 32),
            trades=tuple(Trade.decode(raw) for raw in m.messages(4)),
        )

    def px(self, trade: Trade) -> str:
        return format_fixed(trade.px, self.px_decimals)

    def sz(self, trade: Trade) -> str:
        return format_fixed(trade.sz, self.sz_decimals)


@dataclass(frozen=True)
class JsonFrame:
    """A JSON frame, byte-identical to the JSON endpoint: ``{"channel":..,"data":..}``.

    ``message`` is the parsed document (``None`` if it was not valid JSON),
    ``channel`` and ``data`` its members. This is also the form every stream
    without a typed frame arrives in, and the fallback of the typed streams.
    """

    raw: bytes

    @property
    def message(self) -> Any:
        try:
            return json.loads(self.raw)
        except ValueError:
            return None

    @property
    def channel(self) -> str | None:
        message = self.message
        channel = message.get("channel") if isinstance(message, dict) else None
        return channel if isinstance(channel, str) else None

    @property
    def data(self) -> Any:
        message = self.message
        return message.get("data") if isinstance(message, dict) else None

    def encode(self) -> bytes:
        return self.raw


ServerBody = L2Book | L2Delta | Bbo | Candle | Trades | JsonFrame | LoginResult

_DECODERS: dict[int, Any] = {
    KIND_JSON: JsonFrame,
    KIND_L2_BOOK: L2Book.decode,
    KIND_L2_DELTA: L2Delta.decode,
    KIND_BBO: Bbo.decode,
    KIND_CANDLE: Candle.decode,
    KIND_TRADES: Trades.decode,
    KIND_LOGIN: LoginResult.decode,
}
_KIND_OF: dict[type, int] = {
    JsonFrame: KIND_JSON,
    L2Book: KIND_L2_BOOK,
    L2Delta: KIND_L2_DELTA,
    Bbo: KIND_BBO,
    Candle: KIND_CANDLE,
    Trades: KIND_TRADES,
    LoginResult: KIND_LOGIN,
}


@dataclass(frozen=True)
class ServerFrame:
    """A decoded ``WsV2ServerFrame``: the outer request id and a typed body."""

    id: int
    body: ServerBody

    def encode(self) -> bytes:
        return pb.enc_varint(1, self.id) + pb.enc_bytes_always(
            _KIND_OF[type(self.body)], self.body.encode()
        )


def decode_server_frame(data: bytes) -> ServerFrame:
    """Decode one binary WebSocket message from the server.

    Raises :class:`~riverbit_mm.errors.ProtocolError` if it is not a valid frame.
    """
    msg = pb.Message(data)
    found = [kind for kind in range(KIND_JSON, KIND_LOGIN + 1) if msg.has(kind)]
    if not found:
        raise ProtocolError("server frame has no known body")
    if len(found) > 1:
        raise ProtocolError("server frame has more than one body")
    kind = found[0]
    return ServerFrame(msg.uint(1), _DECODERS[kind](msg.bytes_(kind)))
