"""Hand-written Protobuf wire codec (varint and length-delimited fields only).

This is all bin-ws needs. Users need neither ``protoc`` nor a Protobuf runtime.
The definitions live in ``proto/binws.proto`` at the repository root.
"""

from __future__ import annotations

from collections.abc import Iterable

from .errors import ProtocolError

VARINT = 0
FIXED64 = 1
LEN = 2
FIXED32 = 5


# ---------------------------------------------------------------- encoding


def uvarint(value: int) -> bytes:
    if value < 0 or value >= 1 << 64:
        raise ValueError("varint out of range")
    out = bytearray()
    while value >= 0x80:
        out.append((value & 0x7F) | 0x80)
        value >>= 7
    out.append(value)
    return bytes(out)


def key(field: int, wire: int) -> bytes:
    return uvarint(field << 3 | wire)


def enc_varint(field: int, value: int) -> bytes:
    """Proto3 scalar: a zero value is omitted."""
    return b"" if value == 0 else key(field, VARINT) + uvarint(value)


def enc_varint_always(field: int, value: int) -> bytes:
    """Written even when zero (proto3 ``optional``)."""
    return key(field, VARINT) + uvarint(value)


def enc_bool(field: int, value: bool) -> bytes:
    return enc_varint_always(field, 1) if value else b""


def enc_bytes_always(field: int, data: bytes) -> bytes:
    """Written even when empty: for ``oneof`` members presence is the information."""
    return key(field, LEN) + uvarint(len(data)) + data


def enc_bytes(field: int, data: bytes) -> bytes:
    return b"" if not data else enc_bytes_always(field, data)


def enc_string(field: int, text: str) -> bytes:
    return enc_bytes(field, text.encode("utf-8"))


def enc_packed(field: int, values: Iterable[int]) -> bytes:
    body = b"".join(uvarint(v) for v in values)
    return b"" if not body else enc_bytes_always(field, body)


# ---------------------------------------------------------------- decoding


def read_uvarint(data: bytes, offset: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while shift < 64:
        if offset >= len(data):
            raise ProtocolError("truncated varint")
        byte = data[offset]
        offset += 1
        result |= (byte & 0x7F) << shift
        if byte < 0x80:
            return result, offset
        shift += 7
    raise ProtocolError("invalid varint")


class Message:
    """One decoded message: field number -> occurrences ``(wire_type, value)`` in order."""

    __slots__ = ("fields",)

    def __init__(self, data: bytes) -> None:
        self.fields: dict[int, list[tuple[int, int | bytes]]] = {}
        offset = 0
        size = len(data)
        while offset < size:
            tag, offset = read_uvarint(data, offset)
            number, wire = tag >> 3, tag & 7
            if number == 0:
                raise ProtocolError("protobuf field number 0")
            if wire == VARINT:
                value, offset = read_uvarint(data, offset)
                self.fields.setdefault(number, []).append((wire, value))
            elif wire == LEN:
                length, offset = read_uvarint(data, offset)
                if length > size - offset:
                    raise ProtocolError("truncated length-delimited field")
                self.fields.setdefault(number, []).append((wire, data[offset : offset + length]))
                offset += length
            elif wire == FIXED64:
                if size - offset < 8:
                    raise ProtocolError("truncated fixed64")
                offset += 8
            elif wire == FIXED32:
                if size - offset < 4:
                    raise ProtocolError("truncated fixed32")
                offset += 4
            else:
                raise ProtocolError(f"unsupported protobuf wire type {wire}")

    def has(self, number: int) -> bool:
        return number in self.fields

    def _last(self, number: int) -> tuple[int, int | bytes] | None:
        items = self.fields.get(number)
        return items[-1] if items else None

    def uint(self, number: int, bits: int = 64) -> int:
        item = self._last(number)
        if item is None:
            return 0
        wire, value = item
        if wire != VARINT or not isinstance(value, int):
            raise ProtocolError(f"field {number}: expected a varint")
        if value >> bits:
            raise ProtocolError(f"field {number}: {value} overflows uint{bits}")
        return value

    def opt_uint(self, number: int, bits: int = 64) -> int | None:
        return self.uint(number, bits) if self.has(number) else None

    def boolean(self, number: int) -> bool:
        return self.uint(number) != 0

    def bytes_(self, number: int) -> bytes:
        item = self._last(number)
        if item is None:
            return b""
        wire, value = item
        if wire != LEN or not isinstance(value, bytes):
            raise ProtocolError(f"field {number}: expected bytes")
        return value

    def string(self, number: int) -> str:
        try:
            return self.bytes_(number).decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ProtocolError(f"field {number}: invalid UTF-8") from exc

    def repeated(self, number: int, bits: int = 64) -> list[int]:
        """Repeated varints, packed (wire 2) or unpacked (wire 0), as parsers must accept."""
        out: list[int] = []
        for wire, value in self.fields.get(number, []):
            if wire == VARINT and isinstance(value, int):
                out.append(value)
            elif wire == LEN and isinstance(value, bytes):
                offset = 0
                while offset < len(value):
                    item, offset = read_uvarint(value, offset)
                    out.append(item)
            else:
                raise ProtocolError(f"field {number}: bad wire type in repeated varint")
        if bits < 64 and any(v >> bits for v in out):
            raise ProtocolError(f"field {number}: value overflows uint{bits}")
        return out

    def messages(self, number: int) -> list[bytes]:
        out: list[bytes] = []
        for wire, value in self.fields.get(number, []):
            if wire != LEN or not isinstance(value, bytes):
                raise ProtocolError(f"field {number}: expected an embedded message")
            out.append(value)
        return out
