"""Golden frames (testdata/frames.json) and codec behaviour."""

from __future__ import annotations

import dataclasses
import json
from pathlib import Path
from typing import Any

import pytest

from riverbit_mm import Book, ProtocolError, format_fixed
from riverbit_mm import _proto as pb
from riverbit_mm import frames as fr

_PATH = Path(__file__).resolve().parents[2] / "testdata" / "frames.json"
_FRAMES: list[dict[str, Any]] = json.loads(_PATH.read_text())["frames"]


def _wire(obj: Any) -> Any:
    """Dataclass -> the dict form used by frames.json (bytes as ``*_hex``, None dropped)."""
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        out: dict[str, Any] = {}
        for f in dataclasses.fields(obj):
            value = getattr(obj, f.name)
            if value is None:
                continue
            if isinstance(value, bytes):
                out[f"{f.name}_hex"] = value.hex()
            else:
                out[f.name] = _wire(value)
        return out
    if isinstance(obj, tuple):
        return [_wire(x) for x in obj]
    return obj


def _body_message(body: Any) -> Any:
    if isinstance(body, fr.L2Delta):
        return _wire(body.snapshot or body.update)
    return _wire(body)


_BODY_NAMES = {
    fr.L2Book: "l2_book",
    fr.Bbo: "bbo",
    fr.Candle: "candle",
    fr.Trades: "trades",
    fr.LoginResult: "login",
    fr.JsonFrame: "json",
}


def _server_name(body: Any) -> str:
    if isinstance(body, fr.L2Delta):
        return "l2_delta.snapshot" if body.snapshot is not None else "l2_delta.update"
    return _BODY_NAMES[type(body)]


_CLIENT_BODIES = {
    fr.CLIENT_INFO: "info_json",
    fr.CLIENT_SUBSCRIBE: "subscribe_json",
    fr.CLIENT_UNSUBSCRIBE: "unsubscribe_json",
    fr.CLIENT_PING: "ping",
    fr.CLIENT_LOGIN: "login",
}


@pytest.mark.parametrize("entry", _FRAMES, ids=lambda e: e["name"])
def test_golden_frame(entry: dict[str, Any]) -> None:
    raw = bytes.fromhex(entry["hex"])
    expected = entry["frame"]
    if entry["direction"] == "client":
        cf = fr.decode_client_frame(raw)
        assert cf.id == expected["id"]
        assert _CLIENT_BODIES[cf.body] == expected["body"]
        if cf.body == fr.CLIENT_LOGIN:
            assert _wire(fr.Login.decode(cf.data)) == expected["message"]
        elif cf.body != fr.CLIENT_PING:
            assert cf.data.decode() == expected["json"]
        assert cf.encode() == raw
        return
    sf = fr.decode_server_frame(raw)
    assert sf.id == expected["id"]
    assert _server_name(sf.body) == expected["body"]
    if isinstance(sf.body, fr.JsonFrame):
        assert sf.body.raw.decode() == expected["json"]
    else:
        assert _body_message(sf.body) == expected["message"]
    assert sf.encode() == raw


def _decimal_check(body: Any, decimal: dict[str, Any]) -> None:
    msg: Any = (body.snapshot or body.update) if isinstance(body, fr.L2Delta) else body
    for name, expected in decimal.items():
        if name == "trades":
            got = [{"px": body.px(t), "sz": body.sz(t)} for t in body.trades]
            assert got == expected
        elif name == "spread":
            assert format_fixed(msg.spread, msg.px_decimals) == expected
        elif name.endswith("_px") or name in ("open", "close", "high", "low"):
            value = getattr(msg, name)
            decimals = msg.px_decimals
            rendered = (
                [format_fixed(v, decimals) for v in value]
                if isinstance(value, tuple)
                else format_fixed(value, decimals)
            )
            assert rendered == expected
        else:  # *_sz and volume
            value = getattr(msg, name)
            decimals = msg.sz_decimals
            rendered = (
                [format_fixed(v, decimals) for v in value]
                if isinstance(value, tuple)
                else format_fixed(value, decimals)
            )
            assert rendered == expected


@pytest.mark.parametrize(
    "entry", [e for e in _FRAMES if "decimal" in e["frame"]], ids=lambda e: e["name"]
)
def test_golden_decimals(entry: dict[str, Any]) -> None:
    body = fr.decode_server_frame(bytes.fromhex(entry["hex"])).body
    _decimal_check(body, entry["frame"]["decimal"])


def test_golden_book_replay() -> None:
    by_name = {e["name"]: e for e in _FRAMES}
    checked = 0
    for entry in _FRAMES:
        if "book_after" not in entry:
            continue
        book = Book("BTC")
        chain = []
        node: dict[str, Any] | None = entry
        while node is not None:
            chain.append(node)
            node = by_name.get(node.get("applies_to", ""))
        for step in reversed(chain):
            body = fr.decode_server_frame(bytes.fromhex(step["hex"])).body
            assert book.apply(body)
        expected = entry["book_after"]
        assert [(lvl.px_text, lvl.sz_text) for lvl in book.bids] == [
            (x["px"], x["sz"]) for x in expected["bids"]
        ]
        assert [(lvl.px_text, lvl.sz_text) for lvl in book.asks] == [
            (x["px"], x["sz"]) for x in expected["asks"]
        ]
        checked += 1
    assert checked >= 2


def test_golden_file_is_exercised() -> None:
    assert len(_FRAMES) >= 10


# ---------------------------------------------------------------- codec behaviour


def test_format_fixed() -> None:
    assert format_fixed(1010, 1) == "101.0"
    assert format_fixed(5, 3) == "0.005"
    assert format_fixed(0, 2) == "0.00"
    assert format_fixed(42, 0) == "42"
    assert format_fixed(2**64 - 1, 5) == "184467440737095.51615"


def test_varint_round_trip_and_limits() -> None:
    for value in (0, 1, 127, 128, 300, 2**32, 2**64 - 1):
        encoded = pb.uvarint(value)
        assert pb.read_uvarint(encoded, 0) == (value, len(encoded))
    with pytest.raises(ProtocolError):
        pb.read_uvarint(b"\x80", 0)
    with pytest.raises(ProtocolError):
        pb.read_uvarint(b"\xff" * 10 + b"\x01", 0)


def test_repeated_accepts_unpacked_encoding() -> None:
    # field 5 (bid_px) written as three separate varints instead of one packed field
    data = pb.enc_string(1, "BTC") + b"".join(pb.enc_varint_always(5, v) for v in (1, 2, 3))
    data += b"".join(pb.enc_varint_always(6, v) for v in (10, 20, 30))
    book = fr.L2Book.decode(data)
    assert book.bid_px == (1, 2, 3) and book.bid_sz == (10, 20, 30)


def test_length_mismatch_is_rejected() -> None:
    data = pb.enc_string(1, "BTC") + pb.enc_packed(5, [1, 2]) + pb.enc_packed(6, [1])
    with pytest.raises(ProtocolError):
        fr.L2Book.decode(data)


def test_decimals_overflow_is_rejected() -> None:
    with pytest.raises(ProtocolError):
        fr.Bbo.decode(pb.enc_varint(3, 2**32))


def test_unknown_wire_types_and_garbage() -> None:
    with pytest.raises(ProtocolError):
        pb.Message(b"\x0b")  # wire type 3 (start group)
    with pytest.raises(ProtocolError):
        pb.Message(b"\x12\x05ab")  # truncated length-delimited field
    with pytest.raises(ProtocolError):
        fr.decode_server_frame(b"")
    with pytest.raises(ProtocolError):
        fr.decode_client_frame(b"")


def test_unknown_fields_are_skipped() -> None:
    # An unknown fixed32 / fixed64 field is skipped, not an error.
    extra = pb.key(15, pb.FIXED32) + b"\0\0\0\0" + pb.key(16, pb.FIXED64) + b"\0" * 8
    frame = fr.decode_server_frame(pb.enc_bytes_always(fr.KIND_JSON, b'{"channel":"x"}') + extra)
    assert isinstance(frame.body, fr.JsonFrame) and frame.body.channel == "x"


def test_optional_zero_is_present() -> None:
    update = fr.L2DeltaUpdate("BTC", 1, 1, 1, spread=0)
    assert fr.L2DeltaUpdate.decode(update.encode()).spread == 0
    assert fr.L2DeltaUpdate.decode(fr.L2DeltaUpdate("BTC", 1, 1, 1).encode()).spread is None
    bbo = fr.Bbo("BTC", 1, 1, 1, bid_px=0, bid_sz=0)
    assert fr.Bbo.decode(bbo.encode()).bid_px == 0


def test_client_frame_rejects_unknown_body_field() -> None:
    raw = pb.enc_varint(1, 7) + pb.enc_bytes_always(2, b"{}")
    with pytest.raises(ProtocolError):
        fr.decode_client_frame(raw)


def test_json_frame_properties() -> None:
    frame = fr.JsonFrame(b'{"channel":"trades","data":[1]}')
    assert frame.channel == "trades" and frame.data == [1]
    bad = fr.JsonFrame(b"not json")
    assert bad.message is None and bad.channel is None and bad.data is None


def test_bbo_helpers() -> None:
    bbo = fr.Bbo("BTC", 1, 1, 5, bid_px=800000, bid_sz=200000)
    assert bbo.bid == ("80000.0", "2.00000") and bbo.ask is None
