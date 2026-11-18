"""Exceptions raised by the SDK.

``ActionRejected``, ``RateLimited`` and ``UnknownOutcome`` are deliberately
distinct. A rejected action did not execute. An unknown outcome may have
executed: reconcile it, never re-sign it under a new nonce.
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .signing import SignedRequest


class RiverBitError(Exception):
    """Base class of every SDK exception."""


class HttpError(RiverBitError):
    """A read request got a non-success HTTP status."""

    def __init__(self, status: int, body: str, path: str = "") -> None:
        self.status = status
        self.body = body
        self.path = path
        super().__init__(f"{path} HTTP {status}: {body[:500]}")


class RateLimited(RiverBitError):
    """The server asked the client to slow down (HTTP 429 or a ``429`` reply).

    ``retry_after`` is the server's ``Retry-After`` in seconds, if it sent one.
    Read clients retry automatically and raise this only after their retries are
    used up. Writes never raise it: a write that meets a 429 has an unknown
    outcome as far as the SDK is concerned (see :class:`UnknownOutcome`).
    """

    def __init__(self, retry_after: float | None = None, message: str = "rate limited") -> None:
        self.retry_after = retry_after
        super().__init__(
            message if retry_after is None else f"{message} (retry after {retry_after}s)"
        )


class ActionRejected(RiverBitError):
    """The whole action was rejected and nothing executed.

    ``code`` is the execution-layer code when the node reported one (for example
    91, 1160, 203, 204, 205, 97). Per-leg rejections do not raise: they appear
    in :attr:`Receipt.legs <riverbit_mm.Receipt>`. The nonce of a rejected action is
    spent; build a new action for any retry.
    """

    def __init__(
        self,
        message: str,
        *,
        code: int | None = None,
        nonce: int | None = None,
        response: Any = None,
        request: SignedRequest | None = None,
    ) -> None:
        self.code = code
        self.nonce = nonce
        self.response = response
        self.request = request
        super().__init__(message if code is None else f"{message} (code {code})")


class UnknownOutcome(RiverBitError):
    """The action may or may not have executed.

    Raised on a timeout, a disconnect, a non-200 answer, a malformed or incomplete
    reply (including one whose statuses do not match the legs sent, or any bin-ws
    reply that cannot be read after the request was written), a duplicate nonce
    (code 205: the first request with that nonce may have executed), or a reply
    that asks the caller to query before deciding.

    Carries what is needed to reconcile: ``nonce`` and ``request``, the signed
    request (``request.body`` holds the exact bytes that were sent). **Never
    re-sign this action under a new nonce.** Query its outcome with
    :func:`riverbit_mm.reconcile.reconcile_unknown`, and keep any conflicting
    writes on hold until it resolves.
    """

    def __init__(
        self,
        reason: str,
        *,
        request: SignedRequest,
        transport: str,
        cause: BaseException | None = None,
        retry_after: float | None = None,
    ) -> None:
        self.reason = reason
        self.request = request
        self.nonce = request.nonce
        self.request_body = request.body
        self.transport = transport
        self.cause = cause
        self.retry_after = retry_after
        super().__init__(f"outcome unknown (nonce {request.nonce}, {transport}): {reason}")


class NotSent(RiverBitError):
    """The request was never written to the socket; nothing can have executed.

    Only raised when the bin-ws session was not connected at submission time.
    The same :class:`~riverbit_mm.SignedRequest` may be submitted again; the
    nonce was not consumed.
    """

    def __init__(self, reason: str, *, request: SignedRequest) -> None:
        self.request = request
        self.nonce = request.nonce
        super().__init__(f"request not sent (nonce {request.nonce}): {reason}")


class ProtocolError(RiverBitError):
    """A frame or payload did not match the documented format."""


class LoginFailed(RiverBitError):
    """The bin-ws login was refused or did not complete."""


class SessionClosed(RiverBitError):
    """The bin-ws session is closed or lost its connection."""


class NotConnected(SessionClosed):
    """A bin-ws request was refused because the session was not connected.

    Nothing was written to the socket.
    """


class RequestTimeout(RiverBitError):
    """No reply arrived for a bin-ws request in time."""


class PostError(RiverBitError):
    """A bin-ws ``action_json`` / ``info_json`` request got an ``error`` reply."""

    def __init__(self, request_id: int, payload: str) -> None:
        self.request_id = request_id
        self.payload = payload
        super().__init__(f"bin-ws request {request_id}: error reply: {payload[:500]}")
