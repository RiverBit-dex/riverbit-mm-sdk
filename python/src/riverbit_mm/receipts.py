"""Parsing of ``/exchange`` replies into per-leg receipts."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from typing import Any, Literal

from .errors import ActionRejected, UnknownOutcome
from .signing import SignedRequest

LegKind = Literal[
    "resting", "filled", "success", "waitingForTrigger", "waitingForFill", "error", "unknown"
]

_OK_KINDS = frozenset({"resting", "filled", "success", "waitingForTrigger", "waitingForFill"})

# Node diagnostics of a status=err reply that may still carry effects, or ask the
# caller to query before deciding. They never prove the action did not execute.
_AMBIGUOUS_HINTS = (
    "query the order status",
    "waiting for its trigger",
    "not verified",
    "already pending",
    "localcommitted",
)
_CODE_RE = re.compile(r"action rejected by execution \(code (\d+)\)")


@dataclass(frozen=True)
class Leg:
    """The outcome of one ``statuses[]`` entry."""

    index: int
    kind: LegKind
    oid: int | None = None
    total_sz: str | None = None  # filled: decimal string
    avg_px: str | None = None  # filled: decimal string
    error: str | None = None
    raw: Any = None

    @property
    def ok(self) -> bool:
        """True for a leg that was accepted (it may still have a resting remainder)."""
        return self.kind in _OK_KINDS


@dataclass(frozen=True)
class Receipt:
    """What the node answered for one request that it did process.

    ``legs`` has one entry per ``statuses[]`` item, in action order. A leg that
    was rejected does not raise: inspect :attr:`failed`. Partial success is not
    rolled back, so never replay the whole action.

    ``status`` is ``"ok"`` or ``"err"``; ``"err"`` with legs means the reply carried
    a ``committedResult`` (some effects may exist). ``acknowledged`` is true for the
    bare ``{"type":"default"}`` answer of a single ``modify``: the new oid is
    not returned, look it up by cloid.
    """

    status: str
    request: SignedRequest
    legs: tuple[Leg, ...]
    expected_legs: int
    acknowledged: bool
    raw: Any

    @property
    def nonce(self) -> int:
        return self.request.nonce

    @property
    def complete(self) -> bool:
        """True if there is exactly one leg per leg of the action.

        A ``PrecisionExceeded`` rejection answers with a single status for the whole
        action, which makes this false.
        """
        return len(self.legs) == self.expected_legs

    @property
    def failed(self) -> tuple[Leg, ...]:
        return tuple(leg for leg in self.legs if not leg.ok)

    @property
    def all_ok(self) -> bool:
        return self.status == "ok" and self.complete and not self.failed

    @property
    def oids(self) -> tuple[int | None, ...]:
        return tuple(leg.oid for leg in self.legs)


def parse_leg(index: int, raw: Any) -> Leg:
    if raw == "success":
        return Leg(index, "success", raw=raw)
    if isinstance(raw, dict) and len(raw) == 1:
        ((key, value),) = raw.items()
        if key == "error" and isinstance(value, str):
            return Leg(index, "error", error=value, raw=raw)
        if key in ("resting", "filled") and isinstance(value, dict):
            oid = value.get("oid")
            return Leg(
                index,
                key,
                oid=oid if isinstance(oid, int) and not isinstance(oid, bool) else None,
                total_sz=value.get("totalSz") if key == "filled" else None,
                avg_px=value.get("avgPx") if key == "filled" else None,
                raw=raw,
            )
        if key in ("waitingForTrigger", "waitingForFill"):
            return Leg(index, key, raw=raw)
    return Leg(index, "unknown", raw=raw)


def _statuses(container: Any) -> list[Any] | None:
    if not isinstance(container, dict):
        return None
    data = container.get("data")
    statuses = data.get("statuses") if isinstance(data, dict) else None
    return statuses if isinstance(statuses, list) and statuses else None


def interpret_reply(
    result: Any,
    *,
    request: SignedRequest,
    action_type: str,
    expected_legs: int,
    transport: str,
) -> Receipt:
    """Turn a decoded ``/exchange`` answer into a :class:`Receipt`.

    Raises :class:`ActionRejected` for a deterministic whole-action rejection and
    :class:`UnknownOutcome` for any answer that does not settle the outcome.
    """

    def unknown(reason: str) -> UnknownOutcome:
        return UnknownOutcome(reason, request=request, transport=transport)

    if not isinstance(result, dict):
        raise unknown("reply is not a JSON object")
    status = result.get("status")
    response = result.get("response")

    if status == "ok":
        statuses = _statuses(response)
        if statuses is not None:
            legs = tuple(parse_leg(i, s) for i, s in enumerate(statuses))
            return Receipt("ok", request, legs, expected_legs, False, result)
        if action_type in ("modify", "batchModify") and response == {"type": "default"}:
            return Receipt("ok", request, (), expected_legs, True, result)
        raise unknown("accepted reply without statuses")

    if status == "err":
        committed = result.get("committedResult")
        if committed is not None:
            statuses = _statuses(committed)
            if statuses is None:
                raise unknown("error reply with an unparseable committedResult")
            legs = tuple(parse_leg(i, s) for i, s in enumerate(statuses))
            return Receipt("err", request, legs, expected_legs, False, result)
        text = response if isinstance(response, str) else json.dumps(response)
        lowered = (text + json.dumps(result)).lower()
        if any(hint in lowered for hint in _AMBIGUOUS_HINTS):
            raise unknown(f"node asks to query before deciding: {text[:300]}")
        match = _CODE_RE.search(text)
        raise ActionRejected(
            text[:500],
            code=int(match.group(1)) if match else None,
            nonce=request.nonce,
            response=response,
            request=request,
        )

    raise unknown(f"unexpected reply status {status!r}")
