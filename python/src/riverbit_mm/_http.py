"""Shared HTTP plumbing: one lazily created ``httpx.AsyncClient``, 429 handling."""

from __future__ import annotations

import asyncio
import email.utils
import time
from collections.abc import Awaitable, Callable
from typing import Any

import httpx

from .errors import HttpError, RateLimited

SleepFn = Callable[[float], Awaitable[None]]

DEFAULT_TIMEOUT = 15.0
MAX_RESPONSE_BYTES = 4 << 20


def parse_retry_after(value: str | None, *, now: float | None = None) -> float | None:
    """Seconds from a ``Retry-After`` header (delta-seconds or HTTP date)."""
    if value is None:
        return None
    text = value.strip()
    if not text:
        return None
    try:
        seconds = float(text)
    except ValueError:
        try:
            when = email.utils.parsedate_to_datetime(text).timestamp()
        except (TypeError, ValueError):
            return None
        seconds = when - (time.time() if now is None else now)
    return max(0.0, seconds)


class HttpCore:
    """POSTs JSON to ``<base_url><path>``.

    Pass ``http_client`` to supply your own ``httpx.AsyncClient`` (proxies,
    custom transport, tests). A client passed in is never closed by the SDK.
    """

    def __init__(
        self,
        base_url: str,
        *,
        http_client: httpx.AsyncClient | None = None,
        timeout: float = DEFAULT_TIMEOUT,
        max_retries: int = 5,
        max_retry_after: float = 60.0,
        sleep: SleepFn | None = None,
    ) -> None:
        if not base_url:
            raise ValueError("base_url is required")
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout
        self.max_retries = max_retries
        self.max_retry_after = max_retry_after
        self._sleep: SleepFn = sleep or asyncio.sleep
        self._client = http_client
        self._owns_client = http_client is None

    @property
    def client(self) -> httpx.AsyncClient:
        if self._client is None:
            self._client = httpx.AsyncClient(timeout=self.timeout)
        return self._client

    async def aclose(self) -> None:
        if self._client is not None and self._owns_client:
            await self._client.aclose()
            self._client = None

    async def post_once(
        self, path: str, body: bytes, timeout: float | None = None
    ) -> httpx.Response:
        """One POST, no retry, no status check."""
        return await self.client.post(
            self.base_url + path,
            content=body,
            headers={"Content-Type": "application/json"},
            timeout=self.timeout if timeout is None else timeout,
        )

    async def post_json(self, path: str, payload: bytes) -> Any:
        """POST a read request; retry on 429 honouring ``Retry-After``.

        Raises :class:`RateLimited` when the retries are used up and
        :class:`HttpError` for any other non-200 status. Network errors
        propagate as ``httpx.HTTPError``: reads may simply be retried.
        """
        delay = 0.25
        for attempt in range(self.max_retries + 1):
            response = await self.post_once(path, payload)
            if response.status_code == 429:
                retry_after = parse_retry_after(response.headers.get("Retry-After"))
                wait = retry_after if retry_after is not None else delay
                if attempt == self.max_retries or wait > self.max_retry_after:
                    raise RateLimited(retry_after, f"{path} HTTP 429")
                await self._sleep(wait)
                delay = min(delay * 2, 8.0)
                continue
            if response.status_code != 200:
                raise HttpError(response.status_code, response.text, path)
            if len(response.content) > MAX_RESPONSE_BYTES:
                raise HttpError(200, "response too large", path)
            try:
                return response.json()
            except ValueError as exc:
                raise HttpError(200, f"invalid JSON: {response.text[:200]}", path) from exc
        raise AssertionError("unreachable")  # pragma: no cover
