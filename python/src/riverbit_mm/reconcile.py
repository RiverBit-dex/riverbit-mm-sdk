"""Reconciling a request whose outcome is unknown.

The helpers here only **read**. They never sign, never resubmit and never
allocate a nonce. An :class:`~riverbit_mm.UnknownOutcome` is resolved by finding
positive evidence in ``orderStatus``; elapsed time, ``unknownOid`` and an empty
open-order list prove nothing. A cancel request is never shown settled by its
target's state (see :class:`Reconciliation`).
"""

from __future__ import annotations

import asyncio
import json
import time
from collections.abc import Awaitable, Callable, Mapping
from dataclasses import dataclass
from typing import Any, Protocol

from .actions import Cloid
from .errors import ProtocolError, UnknownOutcome
from .signing import SignedRequest, normalize_address

#: Order states that end an order's life (``order.status`` of ``orderStatus``).
INACTIVE_STATUSES = frozenset(
    {
        "filled",
        "canceled",
        "rejected",
        "tickRejected",
        "minTradeNtlRejected",
        "reduceOnlyRejected",
        "badAloPxRejected",
        "iocCancelRejected",
        "perpMarginRejected",
        "oracleRejected",
        "insufficientSpotBalanceRejected",
        "selfTradeCanceled",
        "reduceOnlyCanceled",
        "siblingFilledCanceled",
        "scheduledCancel",
        "vaultWithdrawalCanceled",
    }
)
#: Every state the SDK knows. ``triggered`` needs further reconciliation of the resulting order.
KNOWN_STATUSES = INACTIVE_STATUSES | {"open", "triggered"}


class InfoSource(Protocol):
    """Anything that answers info queries: ``InfoClient`` or ``BinWsSession``."""

    async def query(self, body: Mapping[str, Any]) -> Any: ...


@dataclass(frozen=True)
class OrderState:
    """What ``orderStatus`` says about one oid or cloid.

    ``status`` is the current ``order.status`` when ``source == "order"``; for
    ``source == "leg"`` it is derived from the latest operation result of the oid
    (``open``, ``filled`` or ``rejected``), which is **not** the current order
    state: a filled leg may still have a resting remainder. ``source == "none"``
    (``unknownOid``) means nothing was found, which is not proof of non-execution.
    """

    target: str  # "oid=123" or "cloid=0x..."
    status: str  # a known status, or "unknown"
    oid: int | None
    height: int | None
    source: str  # "order" | "leg" | "none"
    raw: Any = None

    @property
    def found(self) -> bool:
        return self.source != "none"

    @property
    def inactive(self) -> bool:
        """True only for a current order object in a terminal state."""
        return self.source == "order" and self.status in INACTIVE_STATUSES


def _target(key: int | str) -> str:
    return f"oid={key}" if isinstance(key, int) else f"cloid={key}"


async def order_state(info: InfoSource, user: str, key: int | Cloid | str) -> OrderState:
    """Query ``orderStatus`` for an oid or a cloid and classify the answer."""
    ident: int | str = key if isinstance(key, int) else str(Cloid.from_hex(str(key)))
    out = await info.query({"type": "orderStatus", "user": normalize_address(user), "oid": ident})
    if not isinstance(out, dict):
        raise ProtocolError("orderStatus reply is not an object")
    height = out.get("height") if isinstance(out.get("height"), int) else None
    status = out.get("status")
    target = _target(ident)
    if status == "unknownOid":
        return OrderState(target, "unknown", None, height, "none", out)
    if status == "legResult":
        leg = out.get("leg")
        if not isinstance(leg, dict):
            raise ProtocolError("legResult without a leg")
        if "error" in leg:
            return OrderState(target, "rejected", None, height, "leg", out)
        for name, derived in (("resting", "open"), ("filled", "filled")):
            body = leg.get(name)
            if isinstance(body, dict) and isinstance(body.get("oid"), int) and body["oid"] != 0:
                return OrderState(target, derived, body["oid"], height, "leg", out)
        raise ProtocolError("nonterminal or invalid legResult")
    if status == "order":
        order = out.get("order")
        inner = order.get("order") if isinstance(order, dict) else None
        state = order.get("status") if isinstance(order, dict) else None
        oid = inner.get("oid") if isinstance(inner, dict) else None
        if not isinstance(state, str) or not isinstance(oid, int) or oid == 0:
            raise ProtocolError("invalid orderStatus order object")
        if isinstance(ident, int) and oid != ident:
            raise ProtocolError("orderStatus returned a different oid")
        return OrderState(
            target, state if state in KNOWN_STATUSES else "unknown", oid, height, "order", out
        )
    raise ProtocolError(f"unexpected orderStatus status {status!r}")


_CANCEL_ACTIONS = frozenset({"cancel", "cancelByCloid"})
#: Actions that leave nothing to look up (no oid, no cloid). Today: the request purchase.
_NO_LOOKUP_ACTIONS = frozenset({"reserveRequestWeight"})


@dataclass(frozen=True)
class Targets:
    """What a signed action is expected to leave behind, derived from its JSON."""

    action_type: str
    placed_cloids: tuple[str, ...]  # new or replacement orders findable by cloid
    placed_replaces: tuple[int | None, ...]  # per placed cloid: the oid it replaces, if any
    untracked_legs: int  # placement legs without a cloid: cannot be queried
    cancel_oids: tuple[int, ...]  # must end up inactive
    cancel_cloids: tuple[str, ...]  # must end up inactive
    replaced_oids: tuple[int, ...]  # old oids of modifications


def targets_of(action_json: bytes) -> Targets:
    """List the oids and cloids needed to reconcile ``action_json``.

    A ``reserveRequestWeight`` purchase has no oid or cloid: it returns empty targets.
    :func:`reconcile_request` never reports such a request as resolved; compare
    ``userRateLimit`` (``requests_surplus`` and ``requests_used``) before and after.
    """
    obj = json.loads(action_json)
    kind = obj["type"]
    placed: list[str] = []
    placed_replaces: list[int | None] = []
    untracked = 0
    cancel_oids: list[int] = []
    cancel_cloids: list[str] = []
    replaced: list[int] = []

    def take(order: Mapping[str, Any], replaces: int | None = None) -> None:
        nonlocal untracked
        if "c" in order:
            placed.append(order["c"])
            placed_replaces.append(replaces)
        else:
            untracked += 1

    if kind == "order":
        for order in obj["orders"]:
            take(order)
    elif kind == "cancel":
        cancel_oids = [c["o"] for c in obj["cancels"]]
    elif kind == "cancelByCloid":
        cancel_cloids = [c["cloid"] for c in obj["cancels"]]
    elif kind == "modify":
        replaced.append(obj["oid"])
        take(obj["order"], obj["oid"])
    elif kind in _NO_LOOKUP_ACTIONS:
        pass
    elif kind == "batchModify":
        for item in obj["modifies"]:
            replaced.append(item["oid"])
            take(item["order"], item["oid"])
    else:
        raise ValueError(f"cannot derive reconciliation targets for action type {kind!r}")
    return Targets(
        kind,
        tuple(placed),
        tuple(placed_replaces),
        untracked,
        tuple(cancel_oids),
        tuple(cancel_cloids),
        tuple(replaced),
    )


@dataclass(frozen=True)
class Reconciliation:
    """The evidence gathered for one request.

    Two things are reported separately:

    * ``targets_settled``: the state of what the request names. Each placement or
      replacement cloid resolves to an order, and each cancellation or replaced
      target is a current order in a terminal state.
    * ``request_final``: evidence about **the request itself**. A placement or a
      modification is shown to have run when its cloid resolves to a new order. A
      cancellation is never shown by order state: its target may have ended through
      a fill or another cancel while this request is still in flight, and an
      in-flight ``cancelByCloid`` can cancel a *new* order that reuses the cloid.
      So ``request_final`` stays false for ``cancel`` and ``cancelByCloid`` unless
      you hold evidence about the request itself (its own receipt).

    ``resolved`` is ``targets_settled and request_final``. ``unresolved`` explains
    what is missing and ``hold_cloids`` lists the cloids the request names while it
    is unresolved: **do not reuse a cloid while any request naming it is
    unresolved** (best: never reuse a cloid). When ``resolved`` is false keep the
    request pending and conflicting writes on hold; query again later. It never
    means "not executed".
    """

    request_nonce: int | None
    placed: tuple[OrderState, ...]
    cancels: tuple[OrderState, ...]
    replaced: tuple[OrderState, ...]
    untracked_legs: int
    resolved: bool
    unresolved: tuple[str, ...]
    targets_settled: bool = False
    request_final: bool = False
    hold_cloids: tuple[str, ...] = ()
    #: False when the request names nothing that can be looked up (a request purchase):
    #: ``resolved`` stays false and polling cannot change that.
    lookup_supported: bool = True


async def reconcile_request(
    info: InfoSource, user: str, request: SignedRequest | bytes
) -> Reconciliation:
    """One reconciliation pass for a signed request or its action JSON (read only)."""
    action_json = request.action_json if isinstance(request, SignedRequest) else request
    nonce = request.nonce if isinstance(request, SignedRequest) else None
    t = targets_of(action_json)
    if t.action_type in _NO_LOOKUP_ACTIONS:
        return Reconciliation(
            nonce,
            (),
            (),
            (),
            0,
            False,
            (
                "a request purchase has no oid or cloid to look up: compare userRateLimit "
                "(requests_surplus and requests_used) before and after, keep the request "
                "pending and never resend it",
            ),
            False,
            False,
            (),
            False,
        )

    placed = [await order_state(info, user, c) for c in t.placed_cloids]
    cancels = [await order_state(info, user, o) for o in t.cancel_oids]
    cancels += [await order_state(info, user, c) for c in t.cancel_cloids]
    replaced = [await order_state(info, user, o) for o in t.replaced_oids]

    unresolved: list[str] = []
    if t.untracked_legs:
        unresolved.append(
            f"{t.untracked_legs} placement leg(s) have no cloid and cannot be looked up; "
            "set a cloid on every order"
        )
    for state, old in zip(placed, t.placed_replaces, strict=True):
        if not state.found or state.status == "unknown":
            unresolved.append(f"{state.target}: no order found yet")
        elif old is not None and state.oid == old:
            # A reused cloid still names the old order until the replacement is visible.
            unresolved.append(f"{state.target}: still resolves to the replaced oid {old}")
    for state in cancels:
        if not state.inactive:
            unresolved.append(f"{state.target}: not yet a terminal current order ({state.status})")
    for state in replaced:
        if not state.inactive:
            unresolved.append(f"{state.target}: replaced order not yet inactive ({state.status})")
    targets_settled = not unresolved
    # Order state says nothing about whether a cancel *request* has run or is still in
    # flight, so a cancel is never final from reconciliation alone.
    request_final = targets_settled and t.action_type not in _CANCEL_ACTIONS
    if targets_settled and not request_final:
        unresolved.append(
            "the cancel request itself is unconfirmed: a terminal target does not show it "
            "executed, and it may still be in flight; keep it pending and do not reuse its cloids"
        )
    resolved = targets_settled and request_final
    return Reconciliation(
        nonce,
        tuple(placed),
        tuple(cancels),
        tuple(replaced),
        t.untracked_legs,
        resolved,
        tuple(unresolved),
        targets_settled,
        request_final,
        () if resolved else t.placed_cloids + t.cancel_cloids,
    )


async def reconcile_unknown(
    info: InfoSource, user: str, outcome: UnknownOutcome | SignedRequest | bytes
) -> Reconciliation:
    """Reconcile an :class:`UnknownOutcome` by oid and cloid.

    ``user`` is the account the action traded for: the master address, or the
    vault address for vault trading. Read only; never resends.
    """
    request = outcome.request if isinstance(outcome, UnknownOutcome) else outcome
    return await reconcile_request(info, user, request)


async def wait_reconciled(
    info: InfoSource,
    user: str,
    outcome: UnknownOutcome | SignedRequest | bytes,
    *,
    timeout: float = 60.0,
    interval: float = 0.5,
    sleep: Callable[[float], Awaitable[None]] = asyncio.sleep,
) -> Reconciliation:
    """Poll :func:`reconcile_unknown` until its targets settle or ``timeout`` seconds pass.

    Returns the last pass either way; check ``.resolved``. A cancel request returns
    as soon as its target is terminal, with ``resolved`` still false (polling cannot
    confirm the request itself). A request purchase returns at once, unresolved: it has
    nothing to look up. A timeout is not evidence: leave the request pending.
    """
    deadline = time.monotonic() + timeout
    while True:
        result = await reconcile_unknown(info, user, outcome)
        if result.targets_settled or not result.lookup_supported or time.monotonic() >= deadline:
            return result
        await sleep(interval)
