"""The examples run end to end against fakes (no network, nothing signed leaves the process)."""

from __future__ import annotations

import importlib
import json
import sys
from pathlib import Path
from typing import Any

import httpx
import pytest

from riverbit_mm import Signer
from riverbit_mm import frames as fr

from .mock_ws import MockBinWs, MockConn

EXAMPLES = Path(__file__).resolve().parents[1] / "examples"
HOST = "api.example.test"
MASTER = "0x" + "22" * 20


def load(name: str) -> Any:
    if str(EXAMPLES) not in sys.path:
        sys.path.insert(0, str(EXAMPLES))
    return importlib.import_module(name)


def config(ws_url: str = "ws://127.0.0.1:1/bin-ws") -> Any:
    common = load("_common")
    return common.Config(
        base_url="https://api.example.test",
        ws_url=ws_url,
        login_host=HOST,
        signer=Signer("0x" + "01" * 32),
        vault=None,
    )


def ok_reply(statuses: list[Any]) -> httpx.Response:
    return httpx.Response(200, json={"status": "ok", "response": {"data": {"statuses": statuses}}})


def fake_api() -> tuple[httpx.AsyncClient, list[dict[str, Any]]]:
    """A fake REST API that places, finds and cancels orders."""
    seen: list[dict[str, Any]] = []
    meta = [{"universe": [{"name": "RBT", "szDecimals": 2}, {"name": "BTC", "szDecimals": 5}]}, []]
    oids = iter(range(100, 200))
    state: dict[str, Any] = {"cloids": {}}

    def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        seen.append({"path": request.url.path, "body": body})
        if request.url.path == "/info":
            kind = body["type"]
            if kind == "chainIdentity":
                return httpx.Response(200, json={"nextAgentChainId": 12589})
            if kind == "metaAndAssetCtxs":
                return httpx.Response(200, json=meta)
            if kind == "userRole":
                return httpx.Response(200, json={"role": "agent", "data": {"user": MASTER}})
            if kind == "openOrders":
                return httpx.Response(200, json=[])
            if kind == "orderStatus":
                oid = state["cloids"].get(body["oid"])
                status = "canceled" if oid in state.get("canceled", set()) else "open"
                if oid is None:
                    return httpx.Response(200, json={"status": "unknownOid", "height": 1})
                return httpx.Response(
                    200,
                    json={
                        "status": "order",
                        "height": 9,
                        "order": {"status": status, "order": {"oid": oid}},
                    },
                )
        action = body["action"]
        if action["type"] == "order":
            legs = []
            for order in action["orders"]:
                oid = next(oids)
                state["cloids"][order["c"]] = oid
                legs.append({"resting": {"oid": oid}})
            return httpx.Response(
                200, json={"status": "ok", "response": {"data": {"statuses": legs}}}
            )
        if action["type"] == "cancelByCloid":
            state.setdefault("canceled", set()).update(
                state["cloids"][c["cloid"]] for c in action["cancels"]
            )
        if action["type"] == "batchModify":
            legs = []
            for item in action["modifies"]:
                oid = next(oids)
                state["cloids"][item["order"]["c"]] = oid
                legs.append({"resting": {"oid": oid}})
            return httpx.Response(
                200, json={"status": "ok", "response": {"data": {"statuses": legs}}}
            )
        n = len(action.get("cancels", [1]))
        return ok_reply(["success"] * n)

    return httpx.AsyncClient(transport=httpx.MockTransport(handler)), seen


async def test_place_query_cancel_example(capsys: pytest.CaptureFixture[str]) -> None:
    mod = load("place_query_cancel")
    client, seen = fake_api()
    await mod.run(config(), "BTC", "buy", "50000", "0.001", http_client=client)
    out = capsys.readouterr().out
    assert "asset index 1" in out and "leg 0: resting" in out and "after cancel: canceled" in out
    actions = [s["body"]["action"]["type"] for s in seen if s["path"] == "/exchange"]
    assert actions == ["order", "cancelByCloid"]
    nonces = [s["body"]["nonce"] for s in seen if s["path"] == "/exchange"]
    assert nonces[0] < nonces[1]


async def test_batch_modify_example(capsys: pytest.CaptureFixture[str]) -> None:
    mod = load("batch_modify")
    client, seen = fake_api()
    await mod.run(config(), "BTC", "50000", "51000", "0.001", http_client=client)
    out = capsys.readouterr().out
    assert "oid 100 -> 102" in out and "oid 101 -> 103" in out
    actions = [s["body"]["action"]["type"] for s in seen if s["path"] == "/exchange"]
    assert actions == ["order", "batchModify", "cancel"]
    cancel = [s for s in seen if s["path"] == "/exchange"][2]["body"]["action"]
    assert [c["o"] for c in cancel["cancels"]] == [102, 103]


async def test_stream_example(capsys: pytest.CaptureFixture[str]) -> None:
    mod = load("stream_market_data")
    async with MockBinWs(HOST, user=MASTER) as server:

        async def handler(conn: MockConn, frame: fr.ClientFrame) -> None:
            if frame.body == fr.CLIENT_SUBSCRIBE and b'"l2"' in frame.data:
                snap = fr.L2Book("BTC", 1, 1, 5, (800000,), (200000,), (), (800010,), (100000,), ())
                await conn.send(fr.ServerFrame(0, fr.L2Delta(snapshot=snap)))
                upd = fr.L2DeltaUpdate("BTC", 2, 1, 5, (800005,), (1000,))
                await conn.send(fr.ServerFrame(0, fr.L2Delta(update=upd)))

        server.handler = handler
        await mod.stream(config(server.url), "BTC", max_frames=2)
    out = capsys.readouterr().out
    assert "bid 80000.0 x 2.00000 | ask 80001.0 x 1.00000" in out
    assert "bid 80000.5 x 0.01000 | ask 80001.0 x 1.00000" in out


async def test_reconcile_example(capsys: pytest.CaptureFixture[str]) -> None:
    mod = load("reconcile_after_reconnect")
    client, _ = fake_api()
    async with MockBinWs(HOST, user=MASTER) as server:

        async def drop_soon() -> None:
            await server.wait_for(lambda: bool(server.connections))
            await server.wait_for(lambda: len(server.frames_of(fr.CLIENT_SUBSCRIBE)) == 3)
            await server.connections[0].drop()

        import asyncio

        dropper = asyncio.create_task(drop_soon())
        await asyncio.wait_for(
            mod.run(config(server.url), "BTC", max_events=3, http_client=client), 10
        )
        await dropper
    out = capsys.readouterr().out
    assert "disconnected" in out and "open orders after reconnect: 0" in out
    assert len(server.logins) == 2
