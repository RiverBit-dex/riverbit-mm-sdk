"""Nonce source: millisecond timestamps, strictly increasing under concurrency."""

from __future__ import annotations

import threading
import time
from collections.abc import Callable

_registry: dict[str, NonceSource] = {}
_registry_lock = threading.Lock()


class NonceSource:
    """Hands out strictly increasing millisecond nonces.

    ``next()`` is thread-safe and never blocks on I/O, so it can be called from
    any coroutine or thread. Each signer must use exactly one source: give every
    :class:`~riverbit_mm.ExchangeClient` that uses the same key the same
    instance (see :func:`shared_nonce_source`). A source cannot coordinate
    across processes; run one execution owner per signer.
    """

    def __init__(self, clock_ms: Callable[[], int] | None = None, *, floor: int = 0) -> None:
        self._clock = clock_ms or (lambda: time.time_ns() // 1_000_000)
        self._last = floor
        self._lock = threading.Lock()

    def next(self) -> int:
        with self._lock:
            value = max(self._clock(), self._last + 1)
            self._last = value
            return value

    def observe(self, nonce: int) -> None:
        """Raise the floor, e.g. to the nonce of a request recovered after a restart."""
        with self._lock:
            self._last = max(self._last, nonce)

    @property
    def last(self) -> int:
        """The most recent nonce handed out (0 if none)."""
        return self._last


def shared_nonce_source(signer_address: str) -> NonceSource:
    """One process-wide source per signer address."""
    key = signer_address.lower()
    with _registry_lock:
        source = _registry.get(key)
        if source is None:
            source = _registry[key] = NonceSource()
        return source
