"""Minimal MessagePack encoder for the signing image of an action.

Only the types that appear in RiverBit actions are supported: maps with string
keys, arrays, strings, booleans, null, integers (most compact encoding) and
float64. Map entries are written in insertion order, because key order is part
of the signature.
"""

from __future__ import annotations

import struct
from collections.abc import Mapping, Sequence
from typing import Any


def packb(value: Any) -> bytes:
    out = bytearray()
    _pack(value, out)
    return bytes(out)


def _pack(value: Any, out: bytearray) -> None:
    if value is None:
        out.append(0xC0)
    elif value is True:
        out.append(0xC3)
    elif value is False:
        out.append(0xC2)
    elif isinstance(value, int):
        _pack_int(value, out)
    elif isinstance(value, float):
        out.append(0xCB)
        out += struct.pack(">d", value)
    elif isinstance(value, str):
        _pack_str(value, out)
    elif isinstance(value, Mapping):
        _pack_header(len(value), out, fix=0x80, fix_max=15, w16=0xDE, w32=0xDF)
        for key, item in value.items():
            if not isinstance(key, str):
                raise TypeError("action map keys must be strings")
            _pack_str(key, out)
            _pack(item, out)
    elif isinstance(value, Sequence) and not isinstance(value, (bytes, bytearray)):
        _pack_header(len(value), out, fix=0x90, fix_max=15, w16=0xDC, w32=0xDD)
        for item in value:
            _pack(item, out)
    else:
        raise TypeError(f"cannot encode {type(value).__name__} as MessagePack")


def _pack_header(n: int, out: bytearray, *, fix: int, fix_max: int, w16: int, w32: int) -> None:
    if n <= fix_max:
        out.append(fix | n)
    elif n <= 0xFFFF:
        out.append(w16)
        out += struct.pack(">H", n)
    else:
        out.append(w32)
        out += struct.pack(">I", n)


def _pack_str(text: str, out: bytearray) -> None:
    data = text.encode("utf-8")
    n = len(data)
    if n <= 31:
        out.append(0xA0 | n)
    elif n <= 0xFF:
        out += b"\xd9" + bytes([n])
    elif n <= 0xFFFF:
        out += b"\xda" + struct.pack(">H", n)
    else:
        out += b"\xdb" + struct.pack(">I", n)
    out += data


def _pack_int(n: int, out: bytearray) -> None:
    if n >= 0:
        if n <= 0x7F:
            out.append(n)
        elif n <= 0xFF:
            out += b"\xcc" + bytes([n])
        elif n <= 0xFFFF:
            out += b"\xcd" + struct.pack(">H", n)
        elif n <= 0xFFFFFFFF:
            out += b"\xce" + struct.pack(">I", n)
        elif n <= 0xFFFFFFFFFFFFFFFF:
            out += b"\xcf" + struct.pack(">Q", n)
        else:
            raise OverflowError("integer does not fit in 64 bits")
    else:
        if n >= -32:
            out += struct.pack(">b", n)
        elif n >= -0x80:
            out += b"\xd0" + struct.pack(">b", n)
        elif n >= -0x8000:
            out += b"\xd1" + struct.pack(">h", n)
        elif n >= -0x80000000:
            out += b"\xd2" + struct.pack(">i", n)
        elif n >= -0x8000000000000000:
            out += b"\xd3" + struct.pack(">q", n)
        else:
            raise OverflowError("integer does not fit in 64 bits")
