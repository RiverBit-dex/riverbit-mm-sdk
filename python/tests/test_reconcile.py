"""Unknown-outcome reconciliation helpers (read only, never resign)."""

from __future__ import annotations

import json
from collections.abc import Mapping
from typing import Any

import pytest

from riverbit_mm import (
    BatchModify,
    Cancel,
    CancelByCloid,
    CancelByCloidItem,
    CancelItem,
    Cloid,
    ModifyItem,
    Order,
    Place,
    ProtocolError,
    UnknownOutcome,
    order_state,
    reconcile_unknown,
    wait_reconciled,
)
from riverbit_mm.reconcile import targets_of
from riverbit_mm.signing import SignedRequest, Signer

USER = "0x2222222222222222222222222222222222222222"
C1, C2 = Cloid.from_int(1), Cloid.from_int(2)


class FakeInfo:
    """Answers orderStatus from a table keyed by oid / cloid; counts queries."""

    def __init__(self, table: dict[Any, Any]) -> None:
        self.table = table
        self.queries: list[Mapping[str, Any]] = []

    async def query(self, body: Mapping[str, Any]) -> Any:
        self.queries.append(body)
        assert body["type"] == "orderStatus" and body["user"] == USER
        return self.table.get(body["oid"], {"status": "unknownOid", "height": 3})


def order_reply(oid: int, status: str, height: int = 10) -> dict[str, Any]:
    return {
        "status": "order",
        "height": height,
        "order": {"status": status, "order": {"oid": oid}},
    }


def leg_reply(kind: str, oid: int) -> dict[str, Any]:
    return {"status": "legResult", "height": 10, "leg": {kind: {"oid": oid}}}


def signed(action: Any) -> SignedRequest:
    return Signer("0x" + "01" * 32).sign_action(action.to_json(), 1000, chain_id=1)


async def test_order_state_classification() -> None:
    info = FakeInfo(
        {
            1: order_reply(1, "open"),
            2: order_reply(2, "canceled"),
            3: order_reply(3, "somethingNew"),
            4: leg_reply("resting", 4),
            5: leg_reply("filled", 5),
            6: {"status": "legResult", "height": 1, "leg": {"error": "x"}},
            str(C1): order_reply(9, "open"),
        }
    )
    assert (await order_state(info, USER, 1)).status == "open"
    s2 = await order_state(info, USER, 2)
    assert s2.inactive and s2.oid == 2 and s2.height == 10
    assert (await order_state(info, USER, 3)).status == "unknown"  # never treated as terminal
    s4 = await order_state(info, USER, 4)
    assert (s4.source, s4.status, s4.inactive) == ("leg", "open", False)
    s5 = await order_state(info, USER, 5)
    assert s5.status == "filled" and not s5.inactive  # a leg result is not current state
    assert (await order_state(info, USER, 6)).status == "rejected"
    by_cloid = await order_state(info, USER, C1)
    assert by_cloid.oid == 9 and by_cloid.target == f"cloid={C1}"
    gone = await order_state(info, USER, 99)
    assert not gone.found and gone.status == "unknown" and not gone.inactive


async def test_order_state_rejects_malformed_replies() -> None:
    bad: list[Any] = [
        {"status": "order"},
        {"status": "weird"},
        {"status": "legResult", "leg": {}},
        [],
    ]
    for reply in bad:
        with pytest.raises(ProtocolError):
            await order_state(FakeInfo({1: reply}), USER, 1)
    with pytest.raises(ProtocolError):  # asked for oid 1, got another order back
        await order_state(FakeInfo({1: order_reply(2, "open")}), USER, 1)
    with pytest.raises(ValueError):
        await order_state(FakeInfo({}), USER, "0x" + "A" * 32)


async def test_place_resolved_by_cloid() -> None:
    request = signed(
        Place([Order(0, True, "1", "1", cloid=C1), Order(0, False, "2", "1", cloid=C2)])
    )
    info = FakeInfo({str(C1): order_reply(11, "open"), str(C2): order_reply(12, "filled")})
    result = await reconcile_unknown(info, USER, request)
    assert result.resolved and result.request_nonce == 1000
    assert [s.oid for s in result.placed] == [11, 12]


async def test_unknown_oid_is_never_proof() -> None:
    request = signed(Place([Order(0, True, "1", "1", cloid=C1)]))
    result = await reconcile_unknown(FakeInfo({}), USER, request)
    assert not result.resolved and "no order found" in result.unresolved[0]


async def test_missing_cloid_cannot_be_resolved() -> None:
    request = signed(Place([Order(0, True, "1", "1")]))
    info = FakeInfo({})
    result = await reconcile_unknown(info, USER, request)
    assert not result.resolved and result.untracked_legs == 1 and info.queries == []


async def test_cancel_needs_a_current_inactive_order() -> None:
    request = signed(Cancel([CancelItem(0, 5), CancelItem(0, 6)]))
    done = FakeInfo({5: order_reply(5, "canceled"), 6: order_reply(6, "filled")})
    result = await reconcile_unknown(done, USER, request)
    assert result.targets_settled  # the targets are terminal ...
    assert not result.request_final and not result.resolved  # ... the request is not settled
    assert "cancel request itself is unconfirmed" in result.unresolved[-1]
    open_still = FakeInfo({5: order_reply(5, "canceled"), 6: order_reply(6, "open")})
    result = await reconcile_unknown(open_still, USER, request)
    assert not result.targets_settled and "oid=6" in result.unresolved[0]
    # a leg result alone cannot show the order is no longer live
    legs_only = FakeInfo({5: leg_reply("resting", 5), 6: order_reply(6, "canceled")})
    result = await reconcile_unknown(legs_only, USER, request)
    assert not result.targets_settled and not result.resolved


async def test_cancel_by_cloid_is_never_resolved_by_its_target() -> None:
    request = signed(CancelByCloid([CancelByCloidItem(0, C1)]))
    info = FakeInfo({str(C1): order_reply(7, "canceled")})
    result = await reconcile_unknown(info, USER, request)
    assert info.queries[0]["oid"] == str(C1)
    assert result.targets_settled and not result.request_final and not result.resolved
    assert result.hold_cloids == (str(C1),)  # must not be reused while the request is pending
    # the same answer even though the order is terminal and a new order could reuse the cloid
    again = await reconcile_unknown(FakeInfo({str(C1): order_reply(8, "filled")}), USER, request)
    assert not again.resolved


async def test_resolved_placements_hold_no_cloids() -> None:
    request = signed(Place([Order(0, True, "1", "1", cloid=C1)]))
    result = await reconcile_unknown(FakeInfo({str(C1): order_reply(11, "open")}), USER, request)
    assert result.resolved and result.targets_settled and result.request_final
    assert result.hold_cloids == ()
    pending = await reconcile_unknown(FakeInfo({}), USER, request)
    assert not pending.resolved and pending.hold_cloids == (str(C1),)


async def test_wait_reconciled_returns_for_a_cancel_without_resolving() -> None:
    request = signed(CancelByCloid([CancelByCloidItem(0, C1)]))
    info = FakeInfo({str(C1): order_reply(7, "canceled")})
    sleeps: list[float] = []

    async def fake_sleep(seconds: float) -> None:
        sleeps.append(seconds)

    result = await wait_reconciled(info, USER, request, timeout=60, sleep=fake_sleep)
    assert result.targets_settled and not result.resolved and sleeps == []


async def test_batch_modify_waits_for_the_replacement() -> None:
    order = Order(0, True, "1", "1", cloid=C1)
    request = signed(BatchModify([ModifyItem(20, order)]))
    # the reused cloid still resolves to the old oid: the replacement is not visible yet
    lagging = FakeInfo({str(C1): order_reply(20, "open"), 20: order_reply(20, "open")})
    result = await reconcile_unknown(lagging, USER, request)
    assert not result.resolved
    assert any("still resolves to the replaced oid" in u for u in result.unresolved)
    ok = FakeInfo({str(C1): order_reply(21, "open"), 20: order_reply(20, "canceled")})
    result = await reconcile_unknown(ok, USER, request)
    assert result.resolved and result.placed[0].oid == 21 and result.replaced[0].inactive


async def test_accepts_the_exception_itself() -> None:
    request = signed(Place([Order(0, True, "1", "1", cloid=C1)]))
    outcome = UnknownOutcome("timeout", request=request, transport="rest")
    info = FakeInfo({str(C1): order_reply(11, "open")})
    assert (await reconcile_unknown(info, USER, outcome)).resolved
    assert (await reconcile_unknown(info, USER, request.action_json)).request_nonce is None


async def test_wait_reconciled_polls_until_resolved() -> None:
    request = signed(Place([Order(0, True, "1", "1", cloid=C1)]))
    info = FakeInfo({})
    sleeps: list[float] = []

    async def fake_sleep(seconds: float) -> None:
        sleeps.append(seconds)
        if len(sleeps) == 3:
            info.table[str(C1)] = order_reply(11, "open")

    result = await wait_reconciled(info, USER, request, timeout=60, interval=0.5, sleep=fake_sleep)
    assert result.resolved and sleeps == [0.5, 0.5, 0.5]
    # and returns the last (unresolved) pass when time runs out
    stuck = await wait_reconciled(FakeInfo({}), USER, request, timeout=0, sleep=fake_sleep)
    assert not stuck.resolved


def test_targets_of() -> None:
    t = targets_of(Place([Order(0, True, "1", "1", cloid=C1), Order(0, True, "1", "1")]).to_json())
    assert t.placed_cloids == (str(C1),) and t.untracked_legs == 1
    assert targets_of(Cancel([CancelItem(0, 5)]).to_json()).cancel_oids == (5,)
    with pytest.raises(ValueError):
        targets_of(json.dumps({"type": "withdraw"}).encode())
