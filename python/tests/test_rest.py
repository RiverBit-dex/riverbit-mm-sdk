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
                    {"waitingForFill": {}},
                ]
            },
        },
    }
    client = exchange_for(Recorder(jr(reply)))
    receipt = await client.place([ORDER] * 5)
    assert [leg.kind for leg in receipt.legs] == [
        "resting", "filled", "error", "waitingForTrigger", "waitingForFill",
    ]  # fmt: skip
    assert receipt.legs[1].total_sz == "0.2" and receipt.legs[1].avg_px == "100.5"
    assert receipt.legs[2].error and not receipt.legs[2].ok
    assert [leg.index for leg in receipt.failed] == [2]
    assert not receipt.all_ok and receipt.complete


@pytest.mark.parametrize(
    "response",
    [
        "action rejected by execution (code 205): duplicate nonce",
        "duplicate nonce",
        "action already pending",
    ],
)
async def test_duplicate_nonce_is_an_unknown_outcome(response: str) -> None:
    client = exchange_for(Recorder(jr({"status": "err", "response": response})))
    request = client.sign(Place([ORDER]))
    with pytest.raises(UnknownOutcome) as err:
        await client.submit(request)
    assert err.value.nonce == request.nonce and err.value.request_body == request.body


@pytest.mark.parametrize(
    "statuses",
    [
        [{"filled": {}}],
        [{"filled": {"totalSz": "0.2", "avgPx": "100.5"}}],
        [{"resting": {}}],
        [{"resting": {"oid": 0}}],
        [{"resting": {"oid": True}}],
        ["weird"],
        [{"resting": {"oid": 1}}, {"resting": {"oid": 2}}],  # more statuses than legs
    ],
)
async def test_malformed_or_miscounted_receipts_are_unknown(statuses: list[Any]) -> None:
    reply = {"status": "ok", "response": {"data": {"statuses": statuses}}}
    with pytest.raises(UnknownOutcome):
        await exchange_for(Recorder(jr(reply))).place([ORDER])


async def test_short_receipt_is_unknown_not_success_or_rejection() -> None:
    reply = {"status": "ok", "response": {"data": {"statuses": [{"resting": {"oid": 1}}]}}}
    client = exchange_for(Recorder(jr(reply)))
    request = client.sign(Place([ORDER, ORDER, ORDER]))
    with pytest.raises(UnknownOutcome) as err:
        await client.submit(request)
    assert err.value.request is request
    # the same holds for an error reply that carries a committedResult
    committed = {
        "status": "err",
        "response": "partially applied",
        "committedResult": {"data": {"statuses": [{"resting": {"oid": 9}}]}},
    }
    with pytest.raises(UnknownOutcome):
        await exchange_for(Recorder(jr(committed))).place([ORDER, ORDER])
    # a single-leg action with a precision error is an ordinary rejected leg
    single = {"status": "ok", "response": {"data": {"statuses": [{"error": "PrecisionExceeded"}]}}}
    receipt = await exchange_for(Recorder(jr(single))).place([ORDER])
    assert receipt.complete and not receipt.all_ok and receipt.failed


def test_signed_request_repr_hides_signature_and_body() -> None:
    signer = Signer("0x" + "01" * 32)
    request = signer.sign_action(Place([ORDER]).to_json(), 1_792_168_694_973, chain_id=12589)
    for text in (repr(request), str(request)):
        assert "1792168694973" in text and "'order'" in text and signer.address_lower in text
        assert request.signature.r not in text and request.signature.s not in text
        assert "signature" not in text and "action_json" not in text and "body" not in text
        assert request.body.decode() not in text and request.action_json.decode() not in text
    assert request.signer_address == signer.address_lower
    assert request.body.startswith(b'{"action":')  # the bytes stay available


async def test_receipt_and_unknown_outcome_do_not_leak_the_request() -> None:
    reply = {"status": "ok", "response": {"data": {"statuses": [{"resting": {"oid": 77}}]}}}
    client = exchange_for(Recorder(jr(reply)))
    request = client.sign(Place([ORDER]))
    receipt = await client.submit(request)
    outcome = UnknownOutcome("timeout", request=request, transport="rest")
    rejected = ActionRejected("no", code=91, nonce=request.nonce, request=request)
    for obj in (receipt, outcome, rejected):
        for text in (repr(obj), str(obj)):
            assert request.signature.r not in text and request.signature.s not in text
            assert request.body.decode() not in text
    assert outcome.request_body == request.body  # still available explicitly


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


# ---------------------------------------------------------------- request quota

QUOTA_TEXT = (
    "Too many cumulative requests sent (10045 > 10025) for cumulative volume traded $25.88. "
    "Place taker orders to free up 1 request per USDC traded."
)


async def test_user_rate_limit_query_and_remaining() -> None:
    rec = Recorder(
        jr({"cumVlm": "25.88", "nRequestsCap": 10025, "nRequestsSurplus": 0, "nRequestsUsed": 45})
    )
    async with InfoClient(BASE, http_client=client_for(rec)) as info:
        quota = await info.user_rate_limit(USER)
    assert json.loads(rec.requests[0].content) == {"type": "userRateLimit", "user": USER}
    assert quota.cum_vlm == "25.88" and quota.requests_cap == 10025
    assert quota.requests_used == 45 and quota.requests_surplus == 0
    assert quota.remaining == 9980


def test_user_rate_limit_remaining_saturates_and_adds_surplus() -> None:
    from riverbit_mm import parse_user_rate_limit

    over = parse_user_rate_limit(
        {"cumVlm": "0", "nRequestsCap": 10, "nRequestsUsed": 25, "nRequestsSurplus": 0}
    )
    assert over.remaining == 0
    bought = parse_user_rate_limit(
        {"cumVlm": "0", "nRequestsCap": 10, "nRequestsUsed": 0, "nRequestsSurplus": "500"}
    )
    assert bought.remaining == 510 and bought.requests_surplus == 500
    big = parse_user_rate_limit(
        {
            "cumVlm": "1",
            "nRequestsCap": 2**64 - 1,
            "nRequestsUsed": 0,
            "nRequestsSurplus": 2**64 - 1,
        }
    )
    assert big.remaining == 2 * (2**64 - 1)  # Python ints do not wrap


@pytest.mark.parametrize(
    "answer",
    [
        [],
        {"cumVlm": 5, "nRequestsCap": 1, "nRequestsUsed": 0, "nRequestsSurplus": 0},
        {"cumVlm": "5", "nRequestsCap": -1, "nRequestsUsed": 0, "nRequestsSurplus": 0},
        {"cumVlm": "5", "nRequestsCap": 2**64, "nRequestsUsed": 0, "nRequestsSurplus": 0},
        {"cumVlm": "5", "nRequestsCap": True, "nRequestsUsed": 0, "nRequestsSurplus": 0},
        {"cumVlm": "5", "nRequestsCap": 1, "nRequestsUsed": "x", "nRequestsSurplus": 0},
        {"cumVlm": "5", "nRequestsCap": 1, "nRequestsUsed": 0},
    ],
)
def test_user_rate_limit_rejects_malformed_answers(answer: Any) -> None:
    from riverbit_mm import ProtocolError, parse_user_rate_limit

    with pytest.raises(ProtocolError):
        parse_user_rate_limit(answer)


async def test_user_rate_limit_501_is_the_normal_read_error() -> None:
    async with InfoClient(BASE, http_client=client_for(Recorder(jr({}, 501)))) as info:
        with pytest.raises(HttpError) as err:
            await info.user_rate_limit(USER)
    assert err.value.status == 501


async def test_partly_accepted_action_has_quota_rejected_legs() -> None:
    from riverbit_mm import is_quota_error

    statuses = [
        {"resting": {"oid": 1}},
        {"error": f"action rejected by execution (code 1800) ({QUOTA_TEXT}) asset=3"},
        {"error": QUOTA_TEXT},
        {"error": "PrecisionExceeded"},
    ]
    reply = {"status": "ok", "response": {"data": {"statuses": statuses}}}
    receipt = await exchange_for(Recorder(jr(reply))).place([ORDER] * 4)
    assert [leg.quota_exhausted for leg in receipt.legs] == [False, True, True, False]
    assert [leg.index for leg in receipt.quota_rejected] == [1, 2]
    assert receipt.legs[1].error is not None and is_quota_error(receipt.legs[1].error)
    assert not receipt.all_ok


async def test_modify_quota_rejection_is_final_even_with_committed_result() -> None:
    from riverbit_mm import QUOTA_EXHAUSTED_CODE

    for extra in ({}, {"committedResult": {"data": {"statuses": [{"resting": {"oid": 9}}]}}}):
        reply = {"status": "err", "response": QUOTA_TEXT, **extra}
        with pytest.raises(ActionRejected) as err:
            await exchange_for(Recorder(jr(reply))).modify(5, ORDER)
        assert err.value.quota_exhausted and err.value.code == QUOTA_EXHAUSTED_CODE
        assert QUOTA_TEXT in str(err.value)
        assert not isinstance(err.value, UnknownOutcome)


async def test_wrapped_quota_rejection_keeps_its_code() -> None:
    reply = {
        "status": "err",
        "response": f"action rejected at ingress (code 1800) ({QUOTA_TEXT})",
    }
    with pytest.raises(ActionRejected) as err:
        await exchange_for(Recorder(jr(reply))).place([ORDER])
    assert err.value.code == 1800 and err.value.quota_exhausted


async def test_other_rejections_are_not_quota_rejections() -> None:
    reply = {"status": "err", "response": "action rejected by execution (code 91): unknown signer"}
    with pytest.raises(ActionRejected) as err:
        await exchange_for(Recorder(jr(reply))).place([ORDER])
    assert not err.value.quota_exhausted and not err.value.reserve_overflow


# ---------------------------------------------------------------- reserveRequestWeight


async def test_purchase_success_receipt_has_no_legs() -> None:
    reply = {"status": "ok", "response": {"type": "default"}}
    rec = Recorder(jr(reply))
    receipt = await exchange_for(rec).reserve_request_weight(1000)
    assert receipt.acknowledged and receipt.legs == () and receipt.complete and receipt.all_ok
    body = json.loads(rec.requests[0].content)
    assert body["action"] == {"type": "reserveRequestWeight", "weight": 1000}
    assert "vaultAddress" not in body


async def test_purchase_for_a_vault_paid_by_the_master_uses_destination() -> None:
    reply = {"status": "ok", "response": {"type": "default"}}
    rec = Recorder(jr(reply))
    await exchange_for(rec).reserve_request_weight(5, VAULT)
    body = json.loads(rec.requests[0].content)
    assert body["action"]["destination"] == VAULT and "vaultAddress" not in body


async def test_purchase_with_a_vault_client_pays_from_the_vault() -> None:
    reply = {"status": "ok", "response": {"type": "default"}}
    rec = Recorder(jr(reply))
    client = exchange_for(rec, vault=VAULT)
    await client.reserve_request_weight(5)
    await client.reserve_request_weight(5, VAULT)
    assert all(json.loads(r.content)["vaultAddress"] == VAULT for r in rec.requests)
    with pytest.raises(ValueError, match="destination"):
        await client.reserve_request_weight(5, USER)
    assert len(rec.requests) == 2  # nothing was signed or sent for the refused one


@pytest.mark.parametrize(
    "reply",
    [
        {"status": "err", "response": "insufficient balance"},
        {
            "status": "ok",
            "response": {
                "data": {
                    "statuses": [
                        {
                            "error": "action rejected by execution (code 1801) "
                            "(reserved request weight would overflow)"
                        }
                    ]
                }
            },
        },
    ],
)
async def test_purchase_rejections_are_final(reply: Any) -> None:
    with pytest.raises(ActionRejected) as err:
        await exchange_for(Recorder(jr(reply))).reserve_request_weight(5)
    assert err.value.request is not None
    if "statuses" in json.dumps(reply):
        assert err.value.code == 1801 and err.value.reserve_overflow


@pytest.mark.parametrize(
    "reply",
    [
        {"status": "ok", "response": {"data": {"statuses": ["success"]}}},
        {"status": "ok", "response": {"type": "other"}},
        {"status": "ok"},
        {"status": "err", "response": "action already pending"},
        {"status": "err", "response": "duplicate nonce (code 205)"},
    ],
)
async def test_odd_purchase_replies_are_unknown_outcomes(reply: Any) -> None:
    with pytest.raises(UnknownOutcome):
        await exchange_for(Recorder(jr(reply))).reserve_request_weight(5)


async def test_purchase_timeout_and_5xx_are_unknown_and_never_retried() -> None:
    for reply in (httpx.ReadTimeout("slow"), jr({}, 502)):
        rec = Recorder(reply)
        with pytest.raises(UnknownOutcome):
            await exchange_for(rec).reserve_request_weight(5)
        assert len(rec.requests) == 1


# ---------------------------------------------------------------- gateway credential

TOKEN = "tok-SECRET-123"
HEADER = "X-RiverBit-MM-Token"


async def test_gateway_token_is_sent_on_info_and_exchange_when_set() -> None:
    rec = Recorder(jr({"nextAgentChainId": 1}))
    async with ExchangeClient(
        BASE, Signer("0x" + "01" * 32), http_client=client_for(rec), gateway_token=TOKEN
    ) as client:
        await client.load_chain_id()  # /info through the same core
        rec.replies = [jr({"status": "ok", "response": {"type": "default"}})]
        await client.reserve_request_weight(1)
    assert [r.headers[HEADER] for r in rec.requests] == [TOKEN, TOKEN]
    assert [r.url.path for r in rec.requests] == ["/info", "/exchange"]


@pytest.mark.parametrize("token", [None, ""])
async def test_gateway_token_is_not_sent_when_unset_or_empty(token: str | None) -> None:
    rec = Recorder(jr({}))
    async with InfoClient(BASE, http_client=client_for(rec), gateway_token=token) as info:
        await info.query({"type": "chainIdentity"})
    assert HEADER.lower() not in rec.requests[0].headers


@pytest.mark.parametrize("bad", ["a\r\nb", "a\nb", "a\rb", "tok\u00e9", "a\x00b", "a\tb", "a\x7fb"])
def test_gateway_token_with_header_unsafe_characters_is_refused(bad: str) -> None:
    with pytest.raises(ValueError) as err:
        InfoClient(BASE, gateway_token=bad)
    assert bad not in str(err.value)
    with pytest.raises(ValueError):
        ExchangeClient(BASE, Signer("0x" + "01" * 32), gateway_token=bad)


async def test_gateway_token_is_not_forwarded_across_redirects() -> None:
    seen: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        if request.url.host == "api.example.test":
            return httpx.Response(307, headers={"Location": "https://other.example.test/info"})
        return httpx.Response(200, json={})

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler), follow_redirects=True)
    async with InfoClient(BASE, http_client=http, gateway_token=TOKEN) as info:
        with pytest.raises(HttpError):
            await info.query({"type": "chainIdentity"})
    assert [r.url.host for r in seen] == ["api.example.test"]


async def test_gateway_token_never_appears_in_repr_str_or_errors() -> None:
    rec = Recorder(jr({"x": 1}, 500))
    info = InfoClient(BASE, http_client=client_for(rec), gateway_token=TOKEN)
    exchange = ExchangeClient(
        BASE,
        Signer("0x" + "01" * 32),
        chain_id=1,
        http_client=client_for(Recorder(jr({}, 502))),
        gateway_token=TOKEN,
    )
    texts = [repr(info), str(info), repr(exchange), str(exchange), repr(info._core)]
    with pytest.raises(HttpError) as http_err:
        await info.query({"type": "chainIdentity"})
    texts += [repr(http_err.value), str(http_err.value)]
    with pytest.raises(UnknownOutcome) as unknown:
        await exchange.reserve_request_weight(1)
    texts += [repr(unknown.value), str(unknown.value), repr(unknown.value.request)]
    for text in texts:
        assert TOKEN not in text and "SECRET" not in text
