"""REST clients against a fake HTTP transport (httpx.MockTransport)."""

from __future__ import annotations

import asyncio
import json
from collections.abc import Callable
from typing import Any

import httpx
import pytest

from riverbit_mm import (
    ActionRejected,
    BatchModify,
    Cancel,
    CancelByCloid,
    CancelByCloidItem,
    CancelItem,
    Cloid,
    ExchangeClient,
    HttpError,
    InfoClient,
    Modify,
    ModifyItem,
    NonceSource,
    Order,
    Place,
    RateLimited,
    Signer,
    UnknownOutcome,
    parse_meta,
)

BASE = "https://api.example.test"
USER = "0x2222222222222222222222222222222222222222"
VAULT = "0x1111111111111111111111111111111111111111"
CLOID = str(Cloid.from_int(1))

Handler = Callable[[httpx.Request], httpx.Response]


def client_for(handler: Handler) -> httpx.AsyncClient:
    return httpx.AsyncClient(transport=httpx.MockTransport(handler))


class Recorder:
    def __init__(self, *replies: httpx.Response | Exception) -> None:
        self.replies = list(replies)
        self.requests: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.requests.append(request)
        reply = self.replies.pop(0) if len(self.replies) > 1 else self.replies[0]
        if isinstance(reply, Exception):
            raise reply
        return reply


def jr(obj: Any, status: int = 200, **headers: str) -> httpx.Response:
    return httpx.Response(status, json=obj, headers=headers)


# ---------------------------------------------------------------- info


@pytest.mark.parametrize(
    ("call", "expected"),
    [
        (lambda c: c.chain_identity(), {"type": "chainIdentity"}),
        (lambda c: c.meta_and_asset_ctxs(), {"type": "metaAndAssetCtxs"}),
        (lambda c: c.l2_book("BTC"), {"type": "l2Book", "coin": "BTC"}),
        (
            lambda c: c.user_role(USER.upper().replace("0X", "0x")),
            {"type": "userRole", "user": USER},
        ),
        (lambda c: c.leading_vaults(USER), {"type": "leadingVaults", "user": USER}),
        (lambda c: c.open_orders(USER), {"type": "openOrders", "user": USER}),
        (lambda c: c.frontend_open_orders(USER), {"type": "frontendOpenOrders", "user": USER}),
        (lambda c: c.user_fills(USER), {"type": "userFills", "user": USER}),
        (lambda c: c.clearinghouse_state(USER), {"type": "clearinghouseState", "user": USER}),
        (
            lambda c: c.order_status(USER, 123),
            {"type": "orderStatus", "user": USER, "oid": 123},
        ),
        (
            lambda c: c.order_status(USER, Cloid.from_int(1)),
            {"type": "orderStatus", "user": USER, "oid": CLOID},
        ),
        (
            lambda c: c.candle_snapshot("BTC", "1m", 1000, 2000),
            {
                "type": "candleSnapshot",
                "req": {"coin": "BTC", "interval": "1m", "startTime": 1000, "endTime": 2000},
            },
        ),
    ],
)
async def test_info_request_shapes(
    call: Callable[[InfoClient], Any], expected: dict[str, Any]
) -> None:
    rec = Recorder(jr({"ok": True}))
    async with InfoClient(BASE, http_client=client_for(rec)) as info:
        assert await call(info) == {"ok": True}
    request = rec.requests[0]
    assert request.method == "POST" and str(request.url) == BASE + "/info"
    assert json.loads(request.content) == expected
    assert request.headers["content-type"] == "application/json"


async def test_info_validates_inputs_locally() -> None:
    rec = Recorder(jr({}))
    info = InfoClient(BASE, http_client=client_for(rec))
    with pytest.raises(ValueError):
        await info.open_orders("not-an-address")
    with pytest.raises(ValueError):
        await info.order_status(USER, "0x" + "A" * 32)  # upper-case cloid is refused by the API
    with pytest.raises(TypeError):
        await info.order_status(USER, True)
    assert rec.requests == []


async def test_chain_id_and_assets() -> None:
    meta = [{"universe": [{"name": "RBT", "szDecimals": 2}, {"name": "BTC", "szDecimals": 5}]}, []]
    rec = Recorder(
        jr({"nextAgentChainId": 12589}),
        jr(meta),
    )
    info = InfoClient(BASE, http_client=client_for(rec))
    assert await info.next_agent_chain_id() == 12589
    assets = await info.assets()
    assert assets["BTC"].index == 1 and assets["BTC"].sz_decimals == 5
    assert parse_meta(meta)["RBT"].index == 0


async def test_info_retries_429_after_retry_after() -> None:
    sleeps: list[float] = []

    async def fake_sleep(seconds: float) -> None:
        sleeps.append(seconds)

    rec = Recorder(
        httpx.Response(429, headers={"Retry-After": "2"}),
        httpx.Response(429, headers={"Retry-After": "1.5"}),
        jr([1]),
    )
    info = InfoClient(BASE, http_client=client_for(rec), sleep=fake_sleep)
    assert await info.query({"type": "allMids"}) == [1]
    assert sleeps == [2.0, 1.5] and len(rec.requests) == 3


async def test_info_backs_off_without_retry_after_and_gives_up() -> None:
    sleeps: list[float] = []

    async def fake_sleep(seconds: float) -> None:
        sleeps.append(seconds)

    rec = Recorder(httpx.Response(429))
    info = InfoClient(BASE, http_client=client_for(rec), sleep=fake_sleep, max_retries=3)
    with pytest.raises(RateLimited) as err:
        await info.query({"type": "allMids"})
    assert err.value.retry_after is None
    assert sleeps == [0.25, 0.5, 1.0] and len(rec.requests) == 4


async def test_info_refuses_to_wait_for_a_huge_retry_after() -> None:
    sleeps: list[float] = []

    async def fake_sleep(seconds: float) -> None:
        sleeps.append(seconds)

    rec = Recorder(httpx.Response(429, headers={"Retry-After": "3600"}))
    info = InfoClient(BASE, http_client=client_for(rec), sleep=fake_sleep)
    with pytest.raises(RateLimited) as err:
        await info.query({"type": "allMids"})
    assert err.value.retry_after == 3600 and sleeps == []


async def test_info_http_errors() -> None:
    info = InfoClient(BASE, http_client=client_for(Recorder(httpx.Response(400, text="bad type"))))
    with pytest.raises(HttpError) as err:
        await info.query({"type": "nope"})
    assert err.value.status == 400 and "bad type" in err.value.body
    info = InfoClient(BASE, http_client=client_for(Recorder(httpx.Response(200, text="not json"))))
    with pytest.raises(HttpError):
        await info.query({"type": "x"})


# ---------------------------------------------------------------- exchange


def exchange_for(
    rec: Recorder, *, vault: str | None = None, chain_id: int | None = 12589
) -> ExchangeClient:
    signer = Signer("0x" + "01" * 32)
    return ExchangeClient(
        BASE,
        signer,
        chain_id=chain_id,
        vault_address=vault,
        nonce_source=NonceSource(clock_ms=lambda: 1_792_168_694_973),
        http_client=client_for(rec),
    )


ORDER = Order(0, True, "100.5", "0.2", cloid=CLOID)


async def test_post_sends_exactly_the_signed_bytes() -> None:
    rec = Recorder(
        jr(
            {
                "status": "ok",
                "response": {"type": "order", "data": {"statuses": [{"resting": {"oid": 77}}]}},
            }
        )
    )
    client = exchange_for(rec)
    request = client.sign(Place([ORDER]))
    receipt = await client.submit(request)
    sent = rec.requests[0]
    assert str(sent.url) == BASE + "/exchange" and sent.content == request.body
    assert receipt.all_ok and receipt.oids == (77,) and receipt.nonce == request.nonce
    assert b'"vaultAddress"' not in sent.content


async def test_vault_requests_carry_vault_address() -> None:
    rec = Recorder(jr({"status": "ok", "response": {"data": {"statuses": ["success"]}}}))
    client = exchange_for(rec, vault=VAULT)
    receipt = await client.cancel([CancelItem(0, 5)])
    assert receipt.all_ok
    body = json.loads(rec.requests[0].content)
    assert body["vaultAddress"] == VAULT
    assert body["signature"]["v"] in (27, 28)


async def test_chain_id_is_fetched_once_when_missing() -> None:
    rec = Recorder(
        jr({"nextAgentChainId": 12589}),
        jr({"status": "ok", "response": {"data": {"statuses": [{"resting": {"oid": 1}}]}}}),
    )
    client = exchange_for(rec, chain_id=None)
    with pytest.raises(RuntimeError):
        client.sign(Place([ORDER]))
    await client.post(Place([ORDER]))
    await client.post(Place([ORDER]))
    paths = [r.url.path for r in rec.requests]
    assert paths == ["/info", "/exchange", "/exchange"]
    assert client.chain_id == 12589


async def test_per_leg_statuses_do_not_raise() -> None:
    reply = {
        "status": "ok",
        "response": {
            "type": "order",
            "data": {
                "statuses": [
                    {"resting": {"oid": 11}},
                    {"filled": {"totalSz": "0.2", "avgPx": "100.5", "oid": 12}},
                    {"error": "Post only order would have immediately matched"},
                    {"waitingForTrigger": {}},
                    "weird",
                ]
            },
        },
    }
    client = exchange_for(Recorder(jr(reply)))
    receipt = await client.place([ORDER] * 5)
    assert [leg.kind for leg in receipt.legs] == [
        "resting", "filled", "error", "waitingForTrigger", "unknown",
    ]  # fmt: skip
    assert receipt.legs[1].total_sz == "0.2" and receipt.legs[1].avg_px == "100.5"
    assert receipt.legs[2].error and not receipt.legs[2].ok
    assert [leg.index for leg in receipt.failed] == [2, 4]
    assert not receipt.all_ok and receipt.complete


async def test_single_precision_status_is_flagged_incomplete() -> None:
    reply = {"status": "ok", "response": {"data": {"statuses": [{"error": "PrecisionExceeded"}]}}}
    receipt = await exchange_for(Recorder(jr(reply))).place([ORDER, ORDER, ORDER])
    assert not receipt.complete and not receipt.all_ok


async def test_modify_default_acknowledgement() -> None:
    reply = {"status": "ok", "response": {"type": "default"}}
    receipt = await exchange_for(Recorder(jr(reply))).modify(5, ORDER)
    assert receipt.acknowledged and receipt.legs == ()
    # The same answer to a placement is not understood: unknown outcome.
    with pytest.raises(UnknownOutcome):
        await exchange_for(Recorder(jr(reply))).place([ORDER])


async def test_whole_action_rejection() -> None:
    reply = {"status": "err", "response": "action rejected by execution (code 91): unknown signer"}
    client = exchange_for(Recorder(jr(reply)))
    with pytest.raises(ActionRejected) as err:
        await client.place([ORDER])
    assert err.value.code == 91 and err.value.nonce == 1_792_168_694_973
    assert err.value.request is not None


async def test_plain_error_text_is_a_rejection_without_code() -> None:
    reply = {"status": "err", "response": "Attempted to modify to invalid new order"}
    with pytest.raises(ActionRejected) as err:
        await exchange_for(Recorder(jr(reply))).modify(5, ORDER)
    assert err.value.code is None


@pytest.mark.parametrize(
    "response",
    [
        "action already pending",
        "Please query the order status before retrying",
        "waiting for its trigger",
        "result not verified",
        {"localCommitted": True},
    ],
)
async def test_ambiguous_errors_are_unknown_outcomes(response: Any) -> None:
    client = exchange_for(Recorder(jr({"status": "err", "response": response})))
    with pytest.raises(UnknownOutcome) as err:
        await client.place([ORDER])
    assert err.value.nonce == 1_792_168_694_973 and err.value.request_body


async def test_committed_result_yields_a_receipt() -> None:
    reply = {
        "status": "err",
        "response": "partially applied",
        "committedResult": {
            "type": "order",
            "data": {"statuses": [{"resting": {"oid": 9}}, {"error": "rejected"}]},
        },
    }
    receipt = await exchange_for(Recorder(jr(reply))).place([ORDER, ORDER])
    assert receipt.status == "err" and receipt.oids == (9, None) and not receipt.all_ok
    reply["committedResult"] = {"type": "order"}
    with pytest.raises(UnknownOutcome):
        await exchange_for(Recorder(jr(reply))).place([ORDER, ORDER])


@pytest.mark.parametrize(
    "reply",
    [
        httpx.Response(500, text="boom"),
        httpx.Response(502),
        httpx.Response(200, text="not json"),
        httpx.Response(200, json=[1, 2]),
        httpx.Response(200, json={"status": "weird"}),
        httpx.ReadTimeout("slow"),
        httpx.ConnectError("reset"),
    ],
)
async def test_transport_failures_are_unknown_and_never_retried(reply: Any) -> None:
    rec = Recorder(reply)
    client = exchange_for(rec)
    request = client.sign(Place([ORDER]))
    with pytest.raises(UnknownOutcome) as err:
        await client.submit(request)
    assert len(rec.requests) == 1  # no automatic retry
    assert err.value.nonce == request.nonce
    assert err.value.request_body == request.body == err.value.request.body
    assert err.value.transport == "rest"


async def test_429_on_exchange_is_an_unknown_outcome_not_a_retry() -> None:
    rec = Recorder(httpx.Response(429, headers={"Retry-After": "3"}))
    client = exchange_for(rec)
    with pytest.raises(UnknownOutcome) as err:
        await client.place([ORDER])
    assert err.value.retry_after == 3.0 and len(rec.requests) == 1


async def test_concurrent_posts_get_distinct_increasing_nonces() -> None:
    reply = jr({"status": "ok", "response": {"data": {"statuses": ["success"]}}})
    rec = Recorder(reply)
    client = exchange_for(rec)
    receipts = await asyncio.gather(*(client.cancel([CancelItem(0, i + 1)]) for i in range(50)))
    nonces = [r.nonce for r in receipts]
    assert len(set(nonces)) == 50 and nonces == sorted(nonces)
    assert {json.loads(r.content)["nonce"] for r in rec.requests} == set(nonces)


async def test_all_action_kinds_submit() -> None:
    reply = jr({"status": "ok", "response": {"data": {"statuses": ["success"]}}})
    rec = Recorder(reply)
    client = exchange_for(rec)
    await client.cancel_by_cloid([CancelByCloidItem(0, CLOID)])
    await client.batch_modify([ModifyItem(1, ORDER)])
    for action in (
        Cancel([CancelItem(0, 1)]),
        CancelByCloid([CancelByCloidItem(0, CLOID)]),
        Modify(1, ORDER),
        BatchModify([ModifyItem(1, ORDER)]),
    ):
        await client.post(action)
    types = [json.loads(r.content)["action"]["type"] for r in rec.requests]
    assert types == [
        "cancelByCloid", "batchModify", "cancel", "cancelByCloid", "modify", "batchModify",
    ]  # fmt: skip
