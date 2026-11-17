"""Trading actions with a fixed field order.

Every class serialises to compact JSON with the keys in exactly the order the
node hashes them. Prices and sizes are decimal strings and are never converted
to a number. ``to_json()`` returns the bytes that get signed and sent.
"""

from __future__ import annotations

import json
import re
import secrets
from collections.abc import Sequence
from dataclasses import dataclass, field
from typing import Any, ClassVar, Protocol, runtime_checkable

_CLOID_RE = re.compile(r"^0x[0-9a-f]{32}$")
_TIFS = ("Alo", "Gtc")


@dataclass(frozen=True, order=True)
class Cloid:
    """A 128-bit client order id, written as lowercase ``0x`` plus 32 hex digits."""

    value: bytes

    def __post_init__(self) -> None:
        if not isinstance(self.value, bytes) or len(self.value) != 16:
            raise ValueError("cloid must be 16 bytes")

    @classmethod
    def random(cls) -> Cloid:
        return cls(secrets.token_bytes(16))

    @classmethod
    def from_int(cls, number: int) -> Cloid:
        return cls(number.to_bytes(16, "big"))

    @classmethod
    def from_hex(cls, text: str) -> Cloid:
        """Parse the public form. Upper case and a missing ``0x`` are rejected."""
        if not _CLOID_RE.match(text):
            raise ValueError("cloid must be a lowercase 0x-prefixed 128-bit hex string")
        return cls(bytes.fromhex(text[2:]))

    def __str__(self) -> str:
        return "0x" + self.value.hex()

    def __repr__(self) -> str:
        return f"Cloid('{self}')"


def _check_decimal(name: str, value: object) -> str:
    if not isinstance(value, str):
        raise TypeError(f"{name} must be a decimal string, not {type(value).__name__}")
    if not value:
        raise ValueError(f"{name} must not be empty")
    return value


def _cloid_text(cloid: Cloid | str | None) -> str | None:
    if cloid is None:
        return None
    if isinstance(cloid, Cloid):
        return str(cloid)
    return str(Cloid.from_hex(cloid))


@runtime_checkable
class Action(Protocol):
    """Anything that can be signed and sent."""

    def to_obj(self) -> dict[str, Any]: ...

    def to_json(self) -> bytes: ...


class _ActionBase:
    def to_obj(self) -> dict[str, Any]:  # pragma: no cover - overridden
        raise NotImplementedError

    def to_json(self) -> bytes:
        """Compact JSON in signing key order: the bytes to sign and to send."""
        return json.dumps(self.to_obj(), separators=(",", ":"), ensure_ascii=False).encode("utf-8")


@dataclass(frozen=True)
class Order:
    """One limit order leg.

    ``price`` and ``size`` are decimal strings exactly as they must appear on the
    wire (``"52.000"`` and ``"52"`` sign differently). ``tif`` is ``"Alo"``
    (maker only) or ``"Gtc"``. ``asset`` is the index in ``universe`` from
    ``metaAndAssetCtxs``.
    """

    asset: int
    is_buy: bool
    price: str
    size: str
    reduce_only: bool = False
    tif: str = "Alo"
    cloid: Cloid | str | None = None

    def __post_init__(self) -> None:
        _check_decimal("price", self.price)
        _check_decimal("size", self.size)
        if self.tif not in _TIFS:
            raise ValueError(f"tif must be one of {_TIFS}")
        _cloid_text(self.cloid)

    def to_obj(self) -> dict[str, Any]:
        obj: dict[str, Any] = {
            "a": self.asset,
            "b": self.is_buy,
            "p": self.price,
            "s": self.size,
            "r": self.reduce_only,
            "t": {"limit": {"tif": self.tif}},
        }
        cloid = _cloid_text(self.cloid)
        if cloid is not None:
            obj["c"] = cloid
        return obj

    @property
    def cloid_text(self) -> str | None:
        return _cloid_text(self.cloid)


@dataclass(frozen=True)
class Place(_ActionBase):
    """The ``order`` action: one or more order legs."""

    orders: Sequence[Order]
    grouping: str = "na"
    TYPE: ClassVar[str] = "order"

    def __post_init__(self) -> None:
        if not self.orders:
            raise ValueError("an order action needs at least one order")

    def to_obj(self) -> dict[str, Any]:
        return {
            "type": self.TYPE,
            "orders": [o.to_obj() for o in self.orders],
            "grouping": self.grouping,
        }


@dataclass(frozen=True)
class CancelItem:
    asset: int
    oid: int

    def to_obj(self) -> dict[str, Any]:
        return {"a": self.asset, "o": self.oid}


@dataclass(frozen=True)
class Cancel(_ActionBase):
    """Cancel by order id."""

    cancels: Sequence[CancelItem]
    TYPE: ClassVar[str] = "cancel"

    def __post_init__(self) -> None:
        if not self.cancels:
            raise ValueError("a cancel action needs at least one target")

    def to_obj(self) -> dict[str, Any]:
        return {"type": self.TYPE, "cancels": [c.to_obj() for c in self.cancels]}


@dataclass(frozen=True)
class CancelByCloidItem:
    asset: int
    cloid: Cloid | str

    def to_obj(self) -> dict[str, Any]:
        text = _cloid_text(self.cloid)
        assert text is not None
        return {"asset": self.asset, "cloid": text}


@dataclass(frozen=True)
class CancelByCloid(_ActionBase):
    """Cancel by client order id. The cloid is signed as its exact string."""

    cancels: Sequence[CancelByCloidItem]
    TYPE: ClassVar[str] = "cancelByCloid"

    def __post_init__(self) -> None:
        if not self.cancels:
            raise ValueError("a cancelByCloid action needs at least one target")

    def to_obj(self) -> dict[str, Any]:
        return {"type": self.TYPE, "cancels": [c.to_obj() for c in self.cancels]}


@dataclass(frozen=True)
class Modify(_ActionBase):
    """Replace one order. The acknowledgement does not carry the new oid."""

    oid: int
    order: Order
    TYPE: ClassVar[str] = "modify"

    def to_obj(self) -> dict[str, Any]:
        return {"type": self.TYPE, "oid": self.oid, "order": self.order.to_obj()}


@dataclass(frozen=True)
class ModifyItem:
    oid: int
    order: Order

    def to_obj(self) -> dict[str, Any]:
        return {"oid": self.oid, "order": self.order.to_obj()}


@dataclass(frozen=True)
class BatchModify(_ActionBase):
    """Replace several orders; each leg's receipt carries the new oid."""

    modifies: Sequence[ModifyItem] = field(default_factory=tuple)
    TYPE: ClassVar[str] = "batchModify"

    def __post_init__(self) -> None:
        if not self.modifies:
            raise ValueError("a batchModify action needs at least one modification")

    def to_obj(self) -> dict[str, Any]:
        return {"type": self.TYPE, "modifies": [m.to_obj() for m in self.modifies]}


def action_type(action_json: bytes) -> str:
    """The ``type`` of a serialised action."""
    obj = json.loads(action_json)
    value = obj.get("type") if isinstance(obj, dict) else None
    if not isinstance(value, str):
        raise ValueError("action JSON has no type")
    return value
