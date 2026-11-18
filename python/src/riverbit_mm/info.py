"""REST ``/info`` client."""

from __future__ import annotations

import json
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

import httpx

from ._http import HttpCore, SleepFn
from .actions import Cloid
from .errors import ProtocolError
from .signing import normalize_address

_U64_MAX = (1 << 64) - 1


@dataclass(frozen=True)
class Asset:
    """One entry of ``universe`` from ``metaAndAssetCtxs``."""

    index: int  # the order field ``a``
    name: str
    sz_decimals: int
    raw: Mapping[str, Any]


def parse_meta(meta_and_asset_ctxs: Any) -> dict[str, Asset]:
    """Index the ``universe`` of a ``metaAndAssetCtxs`` answer by coin name."""
    meta = meta_and_asset_ctxs[0] if isinstance(meta_and_asset_ctxs, list) else meta_and_asset_ctxs
    assets: dict[str, Asset] = {}
    for index, entry in enumerate(meta["universe"]):
        assets[entry["name"]] = Asset(index, entry["name"], int(entry["szDecimals"]), entry)
    return assets


def _count(name: str, value: Any) -> int:
    """An unsigned 64-bit count sent as a JSON number or a decimal string."""
    if isinstance(value, bool):
        raise ProtocolError(f"userRateLimit {name} is not a count")
    if isinstance(value, str) and value.isascii() and value.isdigit():
        value = int(value)
    if not isinstance(value, int) or not 0 <= value <= _U64_MAX:
        raise ProtocolError(f"userRateLimit {name} is not an unsigned 64-bit count")
    return value


@dataclass(frozen=True)
class UserRateLimit:
    """The ``userRateLimit`` answer: the request quota of one account.

    ``cum_vlm`` is the cumulative traded volume in USD, kept as the decimal string
    the node sent. ``requests_cap`` is ``10000 + floor(cum_vlm)`` for ordinary
    actions, **without** purchased requests. ``requests_used`` is
    ``max(0, used - purchased)`` and ``requests_surplus`` is
    ``max(0, purchased - used)``: at most one of them is above zero.

    Each account (the master, or a vault when trading for a vault; an API wallet
    counts against the account it acts for) has a cumulative cap of ``10000`` plus
    one request per USD traded. An ``order`` leg, a ``modify``, a ``batchModify``
    entry and a ``cancel`` / ``cancelByCloid`` entry cost one request each. Cancels
    have a higher cap, ``min(cap + 100000, 2 * cap)``. Over the cap, one request is
    still accepted every 10 seconds. Purchased requests raise both caps.
    """

    cum_vlm: str
    requests_cap: int
    requests_used: int
    requests_surplus: int
    raw: Any = None

    @property
    def remaining(self) -> int:
        """Remaining ordinary requests:
        ``max(0, cap - used) + surplus`` (saturating, never negative)."""
        return max(0, self.requests_cap - self.requests_used) + self.requests_surplus


def parse_user_rate_limit(answer: Any) -> UserRateLimit:
    """Parse a ``userRateLimit`` answer; raises :class:`ProtocolError` if malformed."""
    if not isinstance(answer, dict):
        raise ProtocolError("userRateLimit reply is not an object")
    vlm = answer.get("cumVlm")
    if not isinstance(vlm, str):
        raise ProtocolError("userRateLimit cumVlm is not a decimal string")
    return UserRateLimit(
        vlm,
        _count("nRequestsCap", answer.get("nRequestsCap")),
        _count("nRequestsUsed", answer.get("nRequestsUsed")),
        _count("nRequestsSurplus", answer.get("nRequestsSurplus")),
        answer,
    )


def _user(address: str) -> str:
    return normalize_address(address)


class InfoClient:
    """Unsigned read queries over ``POST <base_url>/info``.

    ``base_url`` is explicit configuration, for example ``https://pre-api.riverbit.io``.
    A 429 answer is retried after the server's ``Retry-After`` (or an exponential
    backoff) and raises :class:`~riverbit_mm.RateLimited` once the retries are used
    up. The client also satisfies :class:`riverbit_mm.reconcile.InfoSource`.

    ``gateway_token`` is an optional gateway credential issued to a market maker. When
    set, every request carries it in the ``X-RiverBit-MM-Token`` header. See
    :class:`~riverbit_mm._http.HttpCore`.
    """

    def __init__(
        self,
        base_url: str,
        *,
        http_client: httpx.AsyncClient | None = None,
        timeout: float = 15.0,
        max_retries: int = 5,
        sleep: SleepFn | None = None,
        gateway_token: str | None = None,
        _core: HttpCore | None = None,
    ) -> None:
        self._core = _core or HttpCore(
            base_url,
            http_client=http_client,
            timeout=timeout,
            max_retries=max_retries,
            sleep=sleep,
            gateway_token=gateway_token,
        )

    async def __aenter__(self) -> InfoClient:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        await self._core.aclose()

    async def query(self, body: Mapping[str, Any]) -> Any:
        """POST an arbitrary info query and return the decoded JSON answer."""
        payload = json.dumps(body, separators=(",", ":")).encode("utf-8")
        return await self._core.post_json("/info", payload)

    # -- market and chain ---------------------------------------------------

    async def chain_identity(self) -> Any:
        return await self.query({"type": "chainIdentity"})

    async def next_agent_chain_id(self) -> int:
        """``nextAgentChainId``: the chain id to sign trading actions with."""
        return int((await self.chain_identity())["nextAgentChainId"])

    async def meta_and_asset_ctxs(self) -> Any:
        return await self.query({"type": "metaAndAssetCtxs"})

    async def assets(self) -> dict[str, Asset]:
        """Asset index and ``szDecimals`` by coin. Query it at every startup."""
        return parse_meta(await self.meta_and_asset_ctxs())

    async def l2_book(self, coin: str) -> Any:
        return await self.query({"type": "l2Book", "coin": coin})

    async def candle_snapshot(
        self, coin: str, interval: str, start_time: int, end_time: int | None = None
    ) -> Any:
        req: dict[str, Any] = {"coin": coin, "interval": interval, "startTime": start_time}
        if end_time is not None:
            req["endTime"] = end_time
        return await self.query({"type": "candleSnapshot", "req": req})

    # -- accounts ---------------------------------------------------------------

    async def user_role(self, user: str) -> Any:
        """Resolve an API wallet to its master (weight 60: query once at startup)."""
        return await self.query({"type": "userRole", "user": _user(user)})

    async def leading_vaults(self, master: str) -> Any:
        return await self.query({"type": "leadingVaults", "user": _user(master)})

    async def open_orders(self, user: str) -> Any:
        return await self.query({"type": "openOrders", "user": _user(user)})

    async def frontend_open_orders(self, user: str) -> Any:
        return await self.query({"type": "frontendOpenOrders", "user": _user(user)})

    async def order_status(self, user: str, oid: int | Cloid | str) -> Any:
        """Status of one order by numeric oid or lowercase ``0x`` + 32 hex digit cloid."""
        if isinstance(oid, bool):
            raise TypeError("oid must be an int or a cloid")
        key = oid if isinstance(oid, int) else str(Cloid.from_hex(str(oid)))
        return await self.query({"type": "orderStatus", "user": _user(user), "oid": key})

    async def user_fills(self, user: str) -> Any:
        return await self.query({"type": "userFills", "user": _user(user)})

    async def clearinghouse_state(self, user: str) -> Any:
        return await self.query({"type": "clearinghouseState", "user": _user(user)})

    async def user_rate_limit(self, user: str) -> UserRateLimit:
        """The request quota of ``user``: the master account or a vault, not an API wallet.

        A node that cannot answer replies HTTP 501, raised as :class:`HttpError`.
        """
        answer = await self.query({"type": "userRateLimit", "user": _user(user)})
        return parse_user_rate_limit(answer)
