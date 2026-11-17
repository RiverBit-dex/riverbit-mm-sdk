"""An in-process mock bin-ws server for tests.

It performs the real login check (EIP-191 recovery over the login message) and
then hands every client frame to a test handler. It only listens on 127.0.0.1.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
from collections.abc import Awaitable, Callable
from typing import Any

from websockets.asyncio.server import Server, ServerConnection, serve
from websockets.exceptions import ConnectionClosed

from riverbit_mm import frames as fr
from riverbit_mm.signing import recover_login_signer

Handler = Callable[["MockConn", fr.ClientFrame], Awaitable[None]]


class MockConn:
    def __init__(self, ws: ServerConnection) -> None:
        self.ws = ws

    async def send(self, frame: fr.ServerFrame) -> None:
        await self.ws.send(frame.encode())

    async def send_json(self, request_id: int, obj: Any) -> None:
        raw = obj if isinstance(obj, bytes) else json.dumps(obj, separators=(",", ":")).encode()
        await self.send(fr.ServerFrame(request_id, fr.JsonFrame(raw)))

    async def post_reply(self, request_id: int, kind: str, payload: Any) -> None:
        await self.send_json(
            request_id,
            {
                "channel": "post",
                "data": {"id": request_id, "response": {"type": kind, "payload": payload}},
            },
        )

    async def drop(self) -> None:
        """Close abruptly, as a network failure would."""
        self.ws.transport.abort()


class MockBinWs:
    def __init__(self, login_host: str, *, user: str | None = None) -> None:
        self.login_host = login_host
        self.user = user  # master account reported on login; default: the signer itself
        self.handler: Handler | None = None
        self.reject_login: str | None = None
        self.logins: list[fr.Login] = []
        self.received: list[fr.ClientFrame] = []
        self.connections: list[MockConn] = []
        self._server: Server | None = None
        self.port = 0

    @property
    def url(self) -> str:
        return f"ws://127.0.0.1:{self.port}/bin-ws"

    async def __aenter__(self) -> MockBinWs:
        self._server = await serve(self._serve, "127.0.0.1", 0, max_size=2 << 20)
        self.port = next(iter(self._server.sockets)).getsockname()[1]
        return self

    async def __aexit__(self, *exc: object) -> None:
        assert self._server is not None
        self._server.close()
        await self._server.wait_closed()

    async def _serve(self, ws: ServerConnection) -> None:
        conn = MockConn(ws)
        try:
            raw = await ws.recv()
            assert isinstance(raw, bytes)
            first = fr.decode_client_frame(raw)
            assert first.body == fr.CLIENT_LOGIN, "first client frame must be the login"
            login = fr.Login.decode(first.data)
            self.logins.append(login)
            if self.reject_login is not None:
                await conn.send(fr.ServerFrame(0, fr.LoginResult(False, error=self.reject_login)))
                return
            signer = recover_login_signer(
                self.login_host, login.timestamp_ms, login.nonce, login.signature
            )
            user = self.user or signer
            await conn.send(
                fr.ServerFrame(
                    0, fr.LoginResult(True, bytes.fromhex(user[2:]), bytes.fromhex(signer[2:]))
                )
            )
            self.connections.append(conn)
            async for message in ws:
                assert isinstance(message, bytes)
                frame = fr.decode_client_frame(message)
                self.received.append(frame)
                if frame.body == fr.CLIENT_PING:
                    await conn.send_json(0, {"channel": "pong"})
                elif self.handler is not None:
                    await self.handler(conn, frame)
        except ConnectionClosed:
            return
        finally:
            with contextlib.suppress(Exception):
                await asyncio.sleep(0)

    def frames_of(self, body: int) -> list[fr.ClientFrame]:
        return [f for f in self.received if f.body == body]

    async def wait_for(self, predicate: Callable[[], bool], timeout: float = 5.0) -> None:
        async def poll() -> None:
            while not predicate():
                await asyncio.sleep(0.005)

        await asyncio.wait_for(poll(), timeout)
