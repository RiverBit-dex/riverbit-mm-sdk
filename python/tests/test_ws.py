"""bin-ws session against the in-process mock server."""

from __future__ import annotations

import asyncio
import json
from collections.abc import AsyncIterator
from typing import Any

import pytest

from riverbit_mm import (
    ActionRejected,
    BinWsSession,
    Book,
    Connected,
    Disconnected,
    ExchangeClient,
    LoginFailed,
    NonceSource,
    NotSent,
    Order,
    Place,
    PostError,
    ProtocolError,
    RequestTimeout,
    ServerFrame,
    SessionClosed,
    Signer,
    UnknownOutcome,
    subscriptions,
)
from riverbit_mm import frames as fr
from riverbit_mm.reconcile import reconcile_unknown

from .mock_ws import MockBinWs, MockConn

HOST = "api.example.test"
MASTER = "0x" + "22" * 20


def make_session(server: MockBinWs, signer: Signer, **kw: Any) -> BinWsSession:
    options: dict[str, Any] = {"backoff_initial": 0.01, "backoff_max": 0.05}
    options.update(kw)
    return BinWsSession(server.url, HOST, signer, **options)


async def next_item(items: AsyncIterator[Any], timeout: float = 5.0) -> Any:
    return await asyncio.wait_for(anext(items), timeout)


async def next_frame(items: AsyncIterator[Any]) -> ServerFrame:
    while True:
        item = await next_item(items)
        if isinstance(item, ServerFrame):
            return item


async def test_login_is_the_first_frame(test_signer: Signer) -> None:
    async with MockBinWs(HOST, user=MASTER) as server:
        session = make_session(server, test_signer, clock_ms=lambda: 1_792_168_694_973)
        async with session:
            assert session.connected
            assert session.user == MASTER and session.signer_address == test_signer.address_lower
        login = server.logins[0]
        assert login.timestamp_ms == 1_792_168_694_973 and len(login.nonce) == 16
        assert len(login.signature) == 65 and login.signature[64] in (27, 28)
        # the mock asserts the login is the first frame it receives
        assert server.received == [] or server.received[0].body != fr.CLIENT_LOGIN


async def test_each_login_uses_a_fresh_nonce(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        for _ in range(2):
            async with make_session(server, test_signer):
                pass
        assert server.logins[0].nonce != server.logins[1].nonce


async def test_login_rejected(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        server.reject_login = "bad signature"
        with pytest.raises(LoginFailed, match="bad signature"):
            await make_session(server, test_signer).start()


async def test_wrong_login_host_is_detected(test_signer: Signer) -> None:
    async with MockBinWs("other.example.test") as server:
        with pytest.raises(LoginFailed, match="login host"):
            await make_session(server, test_signer).start()


async def test_subscribe_unsubscribe_and_typed_stream(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            if frame.body == fr.CLIENT_SUBSCRIBE:
                await conn.send_json(0, {"channel": "subscriptionResponse", "data": {"ok": 1}})
                snap = fr.L2Book("BTC", 1, 1, 2, (1000, 990), (100, 200), (), (1010,), (50,), ())
                await conn.send(fr.ServerFrame(0, fr.L2Delta(snapshot=snap)))
                upd = fr.L2DeltaUpdate("BTC", 2, 1, 2, (1005,), (7,), (), (), (1,), ())
                await conn.send(fr.ServerFrame(0, fr.L2Delta(update=upd)))
                bbo = fr.Bbo("BTC", 3, 1, 2, 1005, 7, 1, 1010, 50, 1)
                await conn.send(fr.ServerFrame(0, bbo))
                await conn.send(
                    fr.ServerFrame(0, fr.Trades("BTC", 1, 2, (fr.Trade(True, 1010, 5, 9, 1),)))
                )
                await conn.send(
                    fr.ServerFrame(0, fr.Candle("BTC", "1m", 0, 59999, 1, 2, 1, 2, 3, 0, 9, 1))
                )
                await conn.send(fr.ServerFrame(0, fr.L2Book("BTC", 4, 1, 2)))

        server.handler = handler
        async with make_session(server, test_signer) as session:
            items = session.frames()
            assert isinstance(await next_item(items), Connected)
            await session.subscribe(subscriptions.l2("BTC"))
            book = Book("BTC")
            kinds = []
            for _ in range(7):
                frame = await next_frame(items)
                kinds.append(type(frame.body).__name__)
                book.apply(frame.body)
            assert kinds == ["JsonFrame", "L2Delta", "L2Delta", "Bbo", "Trades", "Candle", "L2Book"]
            assert [(lvl.px_text, lvl.sz_text) for lvl in book.bids] == [
                ("100.5", "0.07"),
                ("100.0", "1.00"),
            ]
            await session.unsubscribe(subscriptions.l2("BTC"))
            await server.wait_for(lambda: bool(server.frames_of(fr.CLIENT_UNSUBSCRIBE)))
        sub = server.frames_of(fr.CLIENT_SUBSCRIBE)[0]
        assert sub.data == b'{"type":"l2","c":"BTC"}' and sub.id == 0
        assert server.frames_of(fr.CLIENT_UNSUBSCRIBE)[0].data == b'{"type":"l2","c":"BTC"}'


async def test_subscription_builders() -> None:
    assert subscriptions.l2_book("BTC", n_sig_figs=3) == {
        "type": "l2Book", "coin": "BTC", "nSigFigs": 3,
    }  # fmt: skip
    assert list(subscriptions.candle("BTC", "1m")) == ["type", "coin", "interval"]
    with pytest.raises(ValueError):
        subscriptions.candle("BTC", "2m")


async def test_ping_and_pong_filtering(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server, make_session(server, test_signer) as session:
        items = session.frames()
        assert isinstance(await next_item(items), Connected)
        await session.ping()
        await server.wait_for(lambda: bool(server.frames_of(fr.CLIENT_PING)))
        await server.connections[0].send_json(0, {"channel": "x"})
        frame = await next_frame(items)  # the pong never shows up before this
        assert isinstance(frame.body, fr.JsonFrame) and frame.body.channel == "x"


async def test_automatic_pings(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server, make_session(server, test_signer, ping_interval=0.02):
        await server.wait_for(lambda: len(server.frames_of(fr.CLIENT_PING)) >= 2)


async def test_requests_are_matched_by_id_even_out_of_order(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        held: list[fr.ClientFrame] = []

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            held.append(frame)
            if len(held) == 3:  # answer in reverse order
                for f in reversed(held):
                    query = json.loads(f.data)
                    await conn.post_reply(f.id, "info", {"type": query["type"], "data": f.id})

        server.handler = handler
        async with make_session(server, test_signer) as session:
            results = await asyncio.gather(
                session.post_info({"type": "openOrders", "user": MASTER}),
                session.post_info({"type": "orderStatus", "user": MASTER, "oid": 1}),
                session.post_info({"type": "clearinghouseState", "user": MASTER}),
            )
        ids = [f.id for f in held]
        assert len(set(ids)) == 3 and 0 not in ids
        assert [r["data"] for r in results] == ids  # each caller got its own reply
        assert [r["type"] for r in results] == ["openOrders", "orderStatus", "clearinghouseState"]


async def test_query_unwraps_data_and_retries_429(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        calls = 0

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            nonlocal calls
            calls += 1
            if calls == 1:
                await conn.post_reply(frame.id, "error", "429 Too Many Requests")
            else:
                await conn.post_reply(frame.id, "info", {"type": "openOrders", "data": [1, 2]})

        server.handler = handler
        async with make_session(server, test_signer) as session:
            assert await session.query({"type": "openOrders", "user": MASTER}) == [1, 2]
            assert calls == 2


async def test_info_error_reply_and_mismatched_payload(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        modes = iter(["error", "mismatch"])

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            if next(modes) == "error":
                await conn.post_reply(frame.id, "error", "unsupported info type")
            else:
                await conn.post_reply(frame.id, "info", {"type": "other", "data": 1})

        server.handler = handler
        async with make_session(server, test_signer) as session:
            with pytest.raises(PostError, match="unsupported"):
                await session.query({"type": "meta"})
            with pytest.raises(ProtocolError):
                await session.query({"type": "meta"})


def exchange_ws(session: BinWsSession, signer: Signer, **kw: Any) -> ExchangeClient:
    return ExchangeClient(
        "https://unused.example.test",
        signer,
        chain_id=12589,
        nonce_source=NonceSource(clock_ms=lambda: 1_792_168_694_973),
        ws=session,
        **kw,
    )


ORDER = Order(0, True, "100.5", "0.2", cloid="0x" + "00" * 15 + "01")


async def test_action_json_carries_the_signed_bytes(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            reply = {"status": "ok", "response": {"data": {"statuses": [{"resting": {"oid": 42}}]}}}
            await conn.post_reply(frame.id, "action", reply)

        server.handler = handler
        async with make_session(server, test_signer) as session:
            client = exchange_ws(session, test_signer)
            request = client.sign(Place([ORDER]))
            receipt = await client.submit(request)
        frame = server.frames_of(fr.CLIENT_ACTION)[0]
        assert frame.data == request.body and frame.id != 0
        assert receipt.all_ok and receipt.oids == (42,)


async def test_rejection_over_bin_ws(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            await conn.post_reply(
                frame.id,
                "action",
                {"status": "err", "response": "action rejected by execution (code 1160)"},
            )

        server.handler = handler
        async with make_session(server, test_signer) as session:
            with pytest.raises(ActionRejected) as err:
                await exchange_ws(session, test_signer).place([ORDER])
        assert err.value.code == 1160


async def test_unanswered_action_is_an_unknown_outcome(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server, make_session(server, test_signer) as session:
        client = exchange_ws(session, test_signer, timeout=0.05)
        request = client.sign(Place([ORDER]))
        with pytest.raises(UnknownOutcome) as err:
            await client.submit(request)
        assert isinstance(err.value.cause, RequestTimeout)
        assert err.value.request_body == request.body and err.value.transport == "bin-ws"


async def test_error_reply_to_an_action_is_an_unknown_outcome(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            await conn.post_reply(frame.id, "error", "429 Too Many Requests")

        server.handler = handler
        async with make_session(server, test_signer) as session:
            with pytest.raises(UnknownOutcome) as err:
                await exchange_ws(session, test_signer).place([ORDER])
        assert isinstance(err.value.cause, PostError)


@pytest.mark.parametrize("variant", ["wrong_type", "inner_id", "no_response", "not_post"])
async def test_malformed_action_reply_is_an_unknown_outcome(
    test_signer: Signer, variant: str
) -> None:
    async with MockBinWs(HOST) as server:

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            if variant == "wrong_type":
                await conn.post_reply(frame.id, "info", {"type": "meta", "data": {}})
            elif variant == "inner_id":
                bad = {"channel": "post", "data": {"id": frame.id + 5, "response": {}}}
                await conn.send_json(frame.id, bad)
            elif variant == "no_response":
                await conn.send_json(frame.id, {"channel": "post", "data": {"id": frame.id}})
            else:
                await conn.send_json(frame.id, {"channel": "other", "data": {}})

        server.handler = handler
        async with make_session(server, test_signer) as session:
            client = exchange_ws(session, test_signer)
            request = client.sign(Place([ORDER]))
            with pytest.raises(UnknownOutcome) as err:
                await client.submit(request)
        assert isinstance(err.value.cause, ProtocolError)
        assert err.value.request is request and err.value.transport == "bin-ws"


async def test_disconnect_while_waiting_is_an_unknown_outcome(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            await conn.drop()

        server.handler = handler
        async with make_session(server, test_signer, reconnect=False) as session:
            with pytest.raises(UnknownOutcome) as err:
                await exchange_ws(session, test_signer).place([ORDER])
        assert isinstance(err.value.cause, SessionClosed)
        assert err.value.nonce == 1_792_168_694_973


async def test_not_connected_means_not_sent(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        session = make_session(server, test_signer)  # never started
        client = exchange_ws(session, test_signer)
        request = client.sign(Place([ORDER]))
        with pytest.raises(NotSent) as err:
            await client.submit(request)
        assert err.value.request is request
        assert server.received == []


async def test_reconnect_logs_in_again_and_resubscribes(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server, make_session(server, test_signer) as session:
        items = session.frames()
        first = await next_item(items)
        assert isinstance(first, Connected) and not first.reconnect
        await session.subscribe(subscriptions.l2("BTC"))
        await session.subscribe(subscriptions.order_updates(MASTER))
        await session.unsubscribe(subscriptions.order_updates(MASTER))
        await server.wait_for(lambda: len(server.frames_of(fr.CLIENT_SUBSCRIBE)) == 2)

        await server.connections[0].drop()
        down = await next_item(items)
        assert isinstance(down, Disconnected) and down.will_reconnect
        up = await next_item(items)
        assert isinstance(up, Connected) and up.reconnect

        assert len(server.logins) == 2  # a fresh login on the new connection
        assert server.logins[0].nonce != server.logins[1].nonce
        await server.wait_for(lambda: len(server.frames_of(fr.CLIENT_SUBSCRIBE)) == 3)
        resent = server.frames_of(fr.CLIENT_SUBSCRIBE)[2]
        assert resent.data == b'{"type":"l2","c":"BTC"}'  # only the live subscription
        assert session.connected


async def test_post_during_reconnect_is_refused_without_sending(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        session = make_session(server, test_signer, backoff_initial=5.0, backoff_max=5.0)
        async with session:
            items = session.frames()
            await next_item(items)
            await server.connections[0].drop()
            assert isinstance(await next_item(items), Disconnected)
            assert not session.connected
            client = exchange_ws(session, test_signer)
            with pytest.raises(NotSent):
                await client.place([ORDER])


async def test_reconnect_login_rejection_is_fatal(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        session = make_session(server, test_signer)
        await session.start()
        items = session.frames()
        await next_item(items)
        server.reject_login = "revoked"
        await server.connections[0].drop()
        with pytest.raises(LoginFailed, match="revoked"):
            async for _ in items:
                pass
        await session.close()


async def test_no_reconnect_when_disabled(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server, make_session(server, test_signer, reconnect=False) as s:
        items = s.frames()
        await next_item(items)
        await server.connections[0].drop()
        down = await next_item(items)
        assert isinstance(down, Disconnected) and not down.will_reconnect
        assert [i async for i in items] == []  # the stream ends
        assert len(server.logins) == 1


async def test_close_ends_the_stream_and_fails_pending(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        session = make_session(server, test_signer)
        await session.start()
        waiting = asyncio.create_task(session.post_info({"type": "openOrders", "user": MASTER}))
        await server.wait_for(lambda: bool(server.frames_of(fr.CLIENT_INFO)))
        await session.close()
        with pytest.raises(SessionClosed):
            await waiting
        remaining = [i async for i in session.frames()]
        assert all(not isinstance(i, ServerFrame) for i in remaining)


async def test_slow_consumer_forces_a_resync(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:
        session = make_session(server, test_signer, max_queue=5)
        await session.start()
        for _ in range(50):
            await server.connections[0].send_json(0, {"channel": "noise"})
        await asyncio.sleep(0.3)  # the consumer does not read while the backlog builds up
        items = session.frames()
        seen_disconnect = False
        reconnected = False

        async def consume() -> None:
            nonlocal seen_disconnect, reconnected
            async for item in items:
                seen_disconnect |= isinstance(item, Disconnected)
                if isinstance(item, Connected) and item.reconnect:
                    reconnected = True
                    return

        await asyncio.wait_for(consume(), 5)
        assert seen_disconnect and reconnected
        await session.close()


async def test_reconcile_over_bin_ws_info(test_signer: Signer) -> None:
    async with MockBinWs(HOST) as server:

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            query = json.loads(frame.data)
            data = {
                "status": "order",
                "height": 5,
                "order": {"status": "open", "order": {"oid": 8}},
            }
            await conn.post_reply(frame.id, "info", {"type": query["type"], "data": data})

        server.handler = handler
        async with make_session(server, test_signer) as session:
            request = ExchangeClient(
                "https://unused.example.test", test_signer, chain_id=12589
            ).sign(Place([ORDER]))
            result = await reconcile_unknown(session, MASTER, request)
        assert result.resolved and result.placed[0].oid == 8
