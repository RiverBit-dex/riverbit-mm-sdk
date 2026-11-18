"""bin-ws session: login, subscriptions, id-matched requests, reconnect.

One :class:`BinWsSession` is one long-lived, logged-in connection that carries
market data, account events and (optionally) signed actions. After a drop it
reconnects, logs in again as the first frame and resubscribes everything that
was subscribed, emitting :class:`Connected` / :class:`Disconnected` events in the
frame stream so you can rebuild local state.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import time
from collections.abc import AsyncIterator, Callable, Mapping
from dataclasses import dataclass
from typing import Any

from websockets.asyncio.client import ClientConnection, connect
from websockets.exceptions import ConnectionClosed, WebSocketException

from . import frames as fr
from .errors import (
    LoginFailed,
    NotConnected,
    PostError,
    ProtocolError,
    RateLimited,
    RequestTimeout,
    SessionClosed,
)
from .signing import Signer, recover_login_signer

log = logging.getLogger("riverbit_mm.ws")

MAX_MESSAGE_BYTES = 2 << 20


@dataclass(frozen=True)
class Connected:
    """The connection is logged in and all subscriptions were (re)sent.

    ``reconnect`` is true after a drop. Subscription acknowledgments follow as
    ``subscriptionResponse`` JSON frames. On a reconnect, reset local books and
    re-read open orders before trusting pushes again.
    """

    login: fr.LoginResult
    reconnect: bool


@dataclass(frozen=True)
class Disconnected:
    """The connection ended. ``error`` says why; the session retries if ``will_reconnect``."""

    error: BaseException | None
    will_reconnect: bool


StreamItem = fr.ServerFrame | Connected | Disconnected


class _Closed:
    pass


_CLOSED = _Closed()


def _json_bytes(obj: Mapping[str, Any]) -> bytes:
    return json.dumps(obj, separators=(",", ":")).encode("utf-8")


class BinWsSession:
    """A logged-in bin-ws connection.

    ``url`` and ``login_host`` are explicit configuration, for example
    ``wss://pre-api.riverbit.io/bin-ws`` and ``pre-api.riverbit.io``; the host
    must match the public server host name.

    Typical use::

        async with BinWsSession(url, host, signer) as session:
            await session.subscribe(subscriptions.l2("BTC"))
            async for item in session.frames():
                ...

    ``frames()`` yields :class:`~riverbit_mm.frames.ServerFrame` objects for pushes,
    acknowledgments and late replies (pongs are dropped), plus :class:`Connected`
    and :class:`Disconnected` events. Replies to :meth:`post_action` and
    :meth:`post_info` go to the waiting caller, matched by id.
    """

    def __init__(
        self,
        url: str,
        login_host: str,
        signer: Signer,
        *,
        reconnect: bool = True,
        ping_interval: float = 20.0,
        login_timeout: float = 10.0,
        request_timeout: float = 20.0,
        backoff_initial: float = 1.0,
        backoff_max: float = 15.0,
        max_queue: int = 10_000,
        clock_ms: Callable[[], int] | None = None,
        connect_kwargs: Mapping[str, Any] | None = None,
    ) -> None:
        if not url or not login_host:
            raise ValueError("url and login_host are required")
        self.url = url
        self.login_host = login_host
        self.signer = signer
        self.reconnect = reconnect
        self.ping_interval = ping_interval
        self.login_timeout = login_timeout
        self.request_timeout = request_timeout
        self.backoff_initial = backoff_initial
        self.backoff_max = backoff_max
        self._clock = clock_ms or (lambda: time.time_ns() // 1_000_000)
        self._connect_kwargs = dict(connect_kwargs or {})
        self._queue: asyncio.Queue[StreamItem | _Closed] = asyncio.Queue(max_queue)
        self._pending: dict[int, asyncio.Future[fr.ServerFrame]] = {}
        self._subs: dict[bytes, None] = {}
        self._next_id = 0
        self._ws: ClientConnection | None = None
        self._ready = False
        self._login: fr.LoginResult | None = None
        self._runner: asyncio.Task[None] | None = None
        self._fatal: BaseException | None = None
        self._closing = False
        self._tasks: set[asyncio.Future[None]] = set()
        self._closed_signalled = False
        self._connected_event = asyncio.Event()

    # ------------------------------------------------------------------ lifecycle

    async def __aenter__(self) -> BinWsSession:
        await self.start()
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.close()

    async def start(self) -> fr.LoginResult:
        """Connect and log in. Raises on failure (:class:`LoginFailed`, ``OSError``, ...)."""
        if self._runner is not None:
            raise RuntimeError("session already started")
        ws, result = await self._open()
        self._runner = asyncio.create_task(self._run(ws, result), name="riverbit-ws-session")
        await self._connected_event.wait()
        return result

    async def close(self) -> None:
        """Close the connection and stop reconnecting. Pending requests fail."""
        self._closing = True
        runner, self._runner = self._runner, None
        if runner is not None:
            runner.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await runner
        ws, self._ws = self._ws, None
        self._ready = False
        if ws is not None:
            with contextlib.suppress(Exception):
                await ws.close()
        self._fail_pending(SessionClosed("session closed"))
        self._signal_closed()

    @property
    def connected(self) -> bool:
        return self._ready

    @property
    def login_result(self) -> fr.LoginResult | None:
        return self._login

    @property
    def user(self) -> str:
        """Master account the server resolved from the login ('' before login)."""
        return self._login.user_address if self._login else ""

    @property
    def signer_address(self) -> str:
        """Address that signed the login: the API wallet, or the master itself."""
        return self._login.signer_address if self._login else ""

    async def wait_connected(self, timeout: float | None = None) -> None:
        """Wait until the session is logged in (useful during a reconnect)."""
        await asyncio.wait_for(self._connected_event.wait(), timeout)

    # ------------------------------------------------------------------ connecting

    async def _open(self) -> tuple[ClientConnection, fr.LoginResult]:
        ws = await connect(
            self.url,
            max_size=MAX_MESSAGE_BYTES,
            ping_interval=None,  # the protocol uses WsV2Ping frames instead
            open_timeout=self.login_timeout,
            **self._connect_kwargs,
        )
        try:
            return ws, await self._do_login(ws)
        except BaseException:
            with contextlib.suppress(Exception):
                await ws.close()
            raise

    async def _do_login(self, ws: ClientConnection) -> fr.LoginResult:
        signed = self.signer.sign_login(self.login_host, self._clock())
        recovered = recover_login_signer(
            self.login_host, signed.timestamp_ms, signed.nonce, signed.signature
        )
        if recovered != self.signer.address_lower:  # pragma: no cover - self check
            raise LoginFailed("local login signature does not recover to the signer")
        login = fr.Login(signed.timestamp_ms, signed.nonce, signed.signature)
        frame = fr.ClientFrame(0, fr.CLIENT_LOGIN, login.encode())
        try:
            await asyncio.wait_for(ws.send(frame.encode()), self.login_timeout)
            raw = await asyncio.wait_for(ws.recv(), self.login_timeout)
        except asyncio.TimeoutError as exc:
            raise LoginFailed("timed out waiting for the login result") from exc
        except ConnectionClosed as exc:
            raise LoginFailed(f"connection closed during login: {exc}") from exc
        if not isinstance(raw, bytes):
            raise LoginFailed("login: server sent a non-binary frame")
        try:
            reply = fr.decode_server_frame(raw)
        except ProtocolError as exc:
            raise LoginFailed(f"undecodable login reply: {exc}") from exc
        result = reply.body
        if not isinstance(result, fr.LoginResult):
            raise LoginFailed("first server frame is not a login result")
        if not result.ok:
            raise LoginFailed(f"login rejected: {result.error}")
        if result.signer_address != self.signer.address_lower:
            raise LoginFailed(
                f"server recovered signer {result.signer_address}, local key is "
                f"{self.signer.address_lower} (check the login host)"
            )
        if len(result.user) != 20:
            raise LoginFailed(f"login result user is {len(result.user)} bytes, want 20")
        return result

    # ------------------------------------------------------------------ run loop

    async def _run(self, ws: ClientConnection, result: fr.LoginResult) -> None:
        first = True
        attempt = 0
        try:
            while True:
                error: BaseException | None = None
                try:
                    await self._serve(ws, result, reconnect=not first)
                except asyncio.CancelledError:
                    raise
                except Exception as exc:
                    error = exc
                finally:
                    self._on_disconnect()
                    with contextlib.suppress(Exception):
                        await ws.close()
                first = False
                will = self.reconnect and not self._closing
                self._emit(Disconnected(error, will))
                if not will:
                    return
                while True:  # reconnect with capped exponential backoff
                    delay = min(self.backoff_initial * 2**attempt, self.backoff_max)
                    attempt += 1
                    await asyncio.sleep(delay)
                    try:
                        ws, result = await self._open()
                        attempt = 0
                        break
                    except LoginFailed as exc:
                        self._fatal = exc
                        self._emit(Disconnected(exc, False))
                        return
                    except (OSError, WebSocketException, asyncio.TimeoutError) as exc:
                        log.warning("reconnect failed: %s", exc)
        finally:
            self._signal_closed()

    async def _serve(
        self, ws: ClientConnection, result: fr.LoginResult, *, reconnect: bool
    ) -> None:
        self._ws, self._login = ws, result
        sent: set[bytes] = set()
        while True:  # subscriptions recorded while we were sending are sent too
            todo = [body for body in self._subs if body not in sent]
            if not todo:
                break
            for body in todo:
                await ws.send(fr.ClientFrame(0, fr.CLIENT_SUBSCRIBE, body).encode())
                sent.add(body)
        self._ready = True
        self._connected_event.set()
        self._emit(Connected(result, reconnect))
        pinger = asyncio.create_task(self._ping_loop(ws))
        read_timeout = 3 * self.ping_interval + 15
        try:
            while True:
                try:
                    raw = await asyncio.wait_for(ws.recv(), read_timeout)
                except asyncio.TimeoutError as exc:
                    raise SessionClosed(
                        "no message from the server; connection presumed dead"
                    ) from exc
                if isinstance(raw, str):
                    continue  # bin-ws only sends binary frames
                self._dispatch(fr.decode_server_frame(raw))
        finally:
            pinger.cancel()
            with contextlib.suppress(asyncio.CancelledError, Exception):
                await pinger

    def _dispatch(self, frame: fr.ServerFrame) -> None:
        if frame.id != 0:
            fut = self._pending.pop(frame.id, None)
            if fut is not None:
                if not fut.done():
                    fut.set_result(frame)
                return
        body = frame.body
        if isinstance(body, fr.JsonFrame) and b'"pong"' in body.raw and body.channel == "pong":
            return
        self._emit(frame)

    async def _ping_loop(self, ws: ClientConnection) -> None:
        while True:
            await asyncio.sleep(self.ping_interval)
            await ws.send(fr.ClientFrame(0, fr.CLIENT_PING, b"").encode())

    def _on_disconnect(self) -> None:
        self._ready = False
        self._ws = None
        self._connected_event.clear()
        self._fail_pending(SessionClosed("connection lost before the reply arrived"))

    def _fail_pending(self, exc: BaseException) -> None:
        pending, self._pending = self._pending, {}
        for fut in pending.values():
            if not fut.done():
                fut.set_exception(exc)

    # ------------------------------------------------------------------ the frame stream

    def _emit(self, item: StreamItem) -> None:
        try:
            self._queue.put_nowait(item)
        except asyncio.QueueFull:
            # A consumer too slow to keep up would silently lose book updates.
            # Drop the backlog and the connection: the resubscription after the
            # reconnect starts every stream from a fresh snapshot.
            log.warning("frame queue full; dropping the connection to resynchronise")
            while not self._queue.empty():
                self._queue.get_nowait()
            self._queue.put_nowait(Disconnected(SessionClosed("consumer too slow"), self.reconnect))
            if self._ws is not None:
                task = asyncio.ensure_future(self._ws.close(1013, "consumer too slow"))
                self._tasks.add(task)
                task.add_done_callback(self._tasks.discard)

    def _signal_closed(self) -> None:
        if self._closed_signalled:
            return
        self._closed_signalled = True
        try:
            self._queue.put_nowait(_CLOSED)
        except asyncio.QueueFull:
            while not self._queue.empty():
                self._queue.get_nowait()
            self._queue.put_nowait(_CLOSED)

    async def frames(self) -> AsyncIterator[StreamItem]:
        """Iterate frames and session events until the session is closed.

        Raises :class:`LoginFailed` if a reconnect was refused (the session then stops).
        """
        while True:
            item = await self._queue.get()
            if isinstance(item, _Closed):
                if self._queue.empty():
                    self._queue.put_nowait(item)  # later frames() calls end as well
                if self._fatal is not None:
                    raise self._fatal
                return
            yield item

    # ------------------------------------------------------------------ requests

    async def _send(self, frame: fr.ClientFrame) -> None:
        ws = self._ws
        if ws is None or not self._ready:
            raise NotConnected("bin-ws session is not connected")
        try:
            await ws.send(frame.encode())
        except ConnectionClosed as exc:
            raise SessionClosed(f"connection closed while sending: {exc}") from exc

    async def subscribe(self, subscription: Mapping[str, Any]) -> None:
        """Subscribe to a stream; it is re-sent automatically after a reconnect.

        While the session is reconnecting the subscription is only recorded.
        """
        body = _json_bytes(subscription)
        self._subs[body] = None
        if self._ready:
            await self._send(fr.ClientFrame(0, fr.CLIENT_SUBSCRIBE, body))

    async def unsubscribe(self, subscription: Mapping[str, Any]) -> None:
        body = _json_bytes(subscription)
        self._subs.pop(body, None)
        if self._ready:
            await self._send(fr.ClientFrame(0, fr.CLIENT_UNSUBSCRIBE, body))

    async def ping(self) -> None:
        """Send a ``WsV2Ping``. The session already pings on its own every ``ping_interval``."""
        await self._send(fr.ClientFrame(0, fr.CLIENT_PING, b""))

    async def _post(self, field: int, data: bytes, want: str, timeout: float | None) -> Any:
        if not self._ready:
            raise NotConnected("bin-ws session is not connected")
        self._next_id += 1
        request_id = self._next_id
        fut: asyncio.Future[fr.ServerFrame] = asyncio.get_running_loop().create_future()
        self._pending[request_id] = fut
        try:
            await self._send(fr.ClientFrame(request_id, field, data))
            try:
                reply = await asyncio.wait_for(
                    fut, self.request_timeout if timeout is None else timeout
                )
            except asyncio.TimeoutError as exc:
                raise RequestTimeout(f"{want} request {request_id}: no reply in time") from exc
        finally:
            self._pending.pop(request_id, None)
        return _parse_post_reply(reply, request_id, want)

    async def post_info(self, query: Mapping[str, Any], *, timeout: float | None = None) -> Any:
        """Send ``info_json``; returns the reply payload ``{"type":..,"data":..}``.

        bin-ws serves only a whitelist of info types; ``metaAndAssetCtxs``,
        ``userRole`` and ``leadingVaults`` must use REST.
        """
        return await self._post(fr.CLIENT_INFO, _json_bytes(query), "info", timeout)

    async def query(self, body: Mapping[str, Any]) -> Any:
        """Like :meth:`post_info` but returns just ``data`` and retries a 429 reply.

        Satisfies :class:`riverbit_mm.reconcile.InfoSource`.
        """
        kind = body.get("type")
        delay = 0.25
        for attempt in range(6):
            try:
                payload = await self.post_info(body)
                break
            except PostError as exc:
                if "429" not in exc.payload:
                    raise
                if attempt == 5:
                    raise RateLimited(None, "bin-ws info request rate limited") from exc
                await asyncio.sleep(delay)
                delay = min(delay * 2, 8.0)
        if not isinstance(payload, dict) or payload.get("type") != kind or "data" not in payload:
            raise ProtocolError(f"unexpected info payload for {kind!r}: {str(payload)[:300]}")
        return payload["data"]


def _parse_post_reply(frame: fr.ServerFrame, request_id: int, want: str) -> Any:
    """``{"channel":"post","data":{"id":N,"response":{"type":"info"|"error","payload":..}}}``"""
    body = frame.body
    if not isinstance(body, fr.JsonFrame):
        raise ProtocolError(f"reply to request {request_id} is not a JSON frame")
    message = body.message
    if not isinstance(message, dict) or message.get("channel") != "post":
        raise ProtocolError(
            f"reply to request {request_id} is not a post reply: {body.raw[:300]!r}"
        )
    data = message.get("data")
    response = data.get("response") if isinstance(data, dict) else None
    if not isinstance(data, dict) or not isinstance(response, dict):
        raise ProtocolError(f"reply to request {request_id} has no response")
    if data.get("id") is not None and data["id"] != request_id:
        raise ProtocolError(f"reply to request {request_id} carries data.id={data['id']}")
    kind = response.get("type")
    if kind == "error":
        payload = response.get("payload")
        raise PostError(request_id, payload if isinstance(payload, str) else json.dumps(payload))
    if kind != want or "payload" not in response:
        raise ProtocolError(f"reply to request {request_id}: response type {kind!r}, want {want!r}")
    return response["payload"]
