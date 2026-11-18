"""Parsing of ``/exchange`` replies into per-leg receipts."""

from __future__ import annotations

import json
import re
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Literal

from .errors import QUOTA_EXHAUSTED_CODE, ActionRejected, UnknownOutcome, is_quota_error
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
    "duplicate nonce",
    "localcommitted",
)
# Execution codes that say the nonce was seen before: the first request with it may
# have executed, so the answer settles nothing about it.
_UNSETTLED_CODES = frozenset({205})
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

    @property
    def quota_exhausted(self) -> bool:
        """True if this leg was rejected for the request quota (final: nothing executed)."""
        return self.kind == "error" and is_quota_error(self.error)


@dataclass(frozen=True)
class Receipt:
    """What the node answered for one request that it did process.

    ``legs`` has one entry per ``statuses[]`` item, in action order, and is only built
    from a well-formed answer (see :func:`interpret_reply`). A leg that
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
    def quota_rejected(self) -> tuple[Leg, ...]:
        """The legs rejected for the request quota. An action can be partly accepted:
        the first legs execute and every later leg carries the quota error."""
        return tuple(leg for leg in self.legs if leg.quota_exhausted)

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
            if not isinstance(oid, int) or isinstance(oid, bool) or oid <= 0:
                # An accepted leg always names its order: without an oid the leg is
                # malformed and says nothing about what happened.
                return Leg(index, "unknown", raw=raw)
            return Leg(
                index,
                key,
                oid=oid,
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


def _is_precision_exceeded(statuses: list[Any]) -> bool:
    """The documented single-status answer that gave no leg an oid."""
    if len(statuses) != 1:
        return False
    only = statuses[0]
    error = only.get("error") if isinstance(only, dict) and len(only) == 1 else None
    return isinstance(error, str) and "PrecisionExceeded" in error


def _reject_or_unknown(
    text: str, raw: Any, *, request: SignedRequest, unknown: Callable[[str], UnknownOutcome]
) -> ActionRejected:
    """The :class:`ActionRejected` for a rejection text, or raise :class:`UnknownOutcome`
    when the text says the outcome is not settled (ambiguous hint, duplicate nonce)."""
    lowered = (text + json.dumps(raw)).lower()
    quota = is_quota_error(text)
    if not quota and any(hint in lowered for hint in _AMBIGUOUS_HINTS):
        raise unknown(f"node asks to query before deciding: {text[:300]}")
    match = _CODE_RE.search(text)
    code = int(match.group(1)) if match else None
    if code is None and quota:
        code = QUOTA_EXHAUSTED_CODE
    if code in _UNSETTLED_CODES:
        raise unknown(f"duplicate nonce (code {code}): the original request may have executed")
    return ActionRejected(text[:500], code=code, nonce=request.nonce, response=raw, request=request)


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
    :class:`UnknownOutcome` for any answer that does not settle the outcome: a
    duplicate nonce (code 205) or ``action already pending``, a receipt with a
    different number of statuses than the action has legs (except the lone
    ``PrecisionExceeded`` status), or a leg that is malformed or lacks its oid.

    A request-quota rejection is final: per leg it is a ``Leg`` with
    :attr:`Leg.quota_exhausted`, for a whole action (``modify``) it raises
    :class:`ActionRejected` with ``quota_exhausted`` true, even if the reply also
    carries a ``committedResult``. A ``reserveRequestWeight`` purchase has no legs
    (``expected_legs == 0``): ``ok`` with ``{"type":"default"}`` is a receipt with
    ``acknowledged`` true, any error entry or an ``err`` reply is a rejection.
    """

    def unknown(reason: str) -> UnknownOutcome:
        return UnknownOutcome(reason, request=request, transport=transport)

    if not isinstance(result, dict):
        raise unknown("reply is not a JSON object")
    status = result.get("status")
    response = result.get("response")

    def legs_of(statuses: list[Any]) -> tuple[Leg, ...]:
        """Parse the legs, or raise unless the receipt is complete and well formed.

        A receipt whose statuses do not line up with the legs of the action, or that
        has a leg that cannot be read, settles nothing: it is neither a success nor
        a rejection. The one documented exception is a lone ``PrecisionExceeded``
        status for a multi-leg action, which means no leg was given an oid.
        """
        legs = tuple(parse_leg(i, s) for i, s in enumerate(statuses))
        if len(legs) != expected_legs:
            if expected_legs > 1 and _is_precision_exceeded(statuses):
                return legs
            raise unknown(
                f"receipt has {len(legs)} status(es) for {expected_legs} leg(s) of the action"
            )
        bad = [leg.index for leg in legs if leg.kind == "unknown"]
        if bad:
            raise unknown(f"receipt leg(s) {bad} are malformed or carry no oid")
        return legs

    if action_type == "reserveRequestWeight" and status == "ok":
        statuses = _statuses(response)
        if statuses is not None:
            errors = [
                e["error"]
                for e in statuses
                if isinstance(e, dict) and isinstance(e.get("error"), str)
            ]
            if errors:
                raise _reject_or_unknown(errors[0], result, request=request, unknown=unknown)
            raise unknown("purchase reply carries statuses without an error")
        if isinstance(response, dict) and response.get("type") == "default":
            return Receipt("ok", request, (), 0, True, result)
        raise unknown("accepted purchase reply is not the default answer")

    if status == "ok":
        statuses = _statuses(response)
        if statuses is not None:
            return Receipt("ok", request, legs_of(statuses), expected_legs, False, result)
        if action_type in ("modify", "batchModify") and response == {"type": "default"}:
            return Receipt("ok", request, (), expected_legs, True, result)
        raise unknown("accepted reply without statuses")

    if status == "err":
        committed = result.get("committedResult")
        text = response if isinstance(response, str) else json.dumps(response)
        # A quota rejection is final even when a committedResult member is present.
        if (
            committed is not None
            and action_type != "reserveRequestWeight"
            and not is_quota_error(text)
        ):
            statuses = _statuses(committed)
            if statuses is None:
                raise unknown("error reply with an unparseable committedResult")
            return Receipt("err", request, legs_of(statuses), expected_legs, False, result)
        raise _reject_or_unknown(text, result, request=request, unknown=unknown)

    raise unknown(f"unexpected reply status {status!r}")
