"""REST ``/info`` client."""

from __future__ import annotations

import json
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

import httpx

from ._http import HttpCore, SleepFn
from .actions import Cloid
from .signing import normalize_address


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


def _user(address: str) -> str:
    return normalize_address(address)


class InfoClient:
    """Unsigned read queries over ``POST <base_url>/info``.

    ``base_url`` is explicit configuration, for example ``https://pre-api.riverbit.io``.
    A 429 answer is retried after the server's ``Retry-After`` (or an exponential
    backoff) and raises :class:`~riverbit_mm.RateLimited` once the retries are used
    up. The client also satisfies :class:`riverbit_mm.reconcile.InfoSource`.
    """

    def __init__(
        self,
        base_url: str,
        *,
        http_client: httpx.AsyncClient | None = None,
        timeout: float = 15.0,
        max_retries: int = 5,
        sleep: SleepFn | None = None,
        _core: HttpCore | None = None,
    ) -> None:
        self._core = _core or HttpCore(
            base_url, http_client=http_client, timeout=timeout, max_retries=max_retries, sleep=sleep
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
