"""Show the request quota of the trading account; optionally buy more requests.

    RIVERBIT_KEY_FILE=./key python examples/quota.py
    BUY_REQUESTS=1000 RIVERBIT_KEY_FILE=./key python examples/quota.py

Without ``BUY_REQUESTS`` the example only sends the unsigned ``userRateLimit`` query and
prints the cap, the requests used, the surplus and what remains. With
``BUY_REQUESTS=<n>`` it also signs and sends ``reserveRequestWeight`` for ``n``
requests, which **spends real USDC** (0.0005 USDC per request, paid by the master
account of the API wallet): use a test account. When ``RIVERBIT_VAULT`` is set the
requests are bought for the vault and still paid by the master (the client is created
without ``vault_address`` and ``destination`` is the vault).

A purchase has no oid, so after an unknown outcome there is nothing to look up: the
example prints the quota again and you compare it with the one printed before. Never
buy again until you know.
"""

from __future__ import annotations

import asyncio
import os

import httpx

from _common import Config, load_config
from riverbit_mm import (
    ActionRejected,
    ExchangeClient,
    InfoClient,
    UnknownOutcome,
    UserRateLimit,
    reserve_cost_micro_usdc,
)


def show(label: str, quota: UserRateLimit) -> None:
    print(
        f"{label}: cumVlm={quota.cum_vlm} cap={quota.requests_cap} used={quota.requests_used} "
        f"surplus={quota.requests_surplus} remaining={quota.remaining}"
    )


async def run(
    cfg: Config, buy: int | None = None, http_client: httpx.AsyncClient | None = None
) -> None:
    async with (
        InfoClient(cfg.base_url, http_client=http_client, gateway_token=cfg.gateway_token) as info,
        ExchangeClient(
            cfg.base_url,
            cfg.signer,
            http_client=http_client,
            gateway_token=cfg.gateway_token,
        ) as exchange,
    ):
        account = cfg.vault
        if account is None:
            role = await info.user_role(cfg.signer.address_lower)
            account = (
                role["data"]["user"] if role.get("role") == "agent" else cfg.signer.address_lower
            )
        before = await info.user_rate_limit(account)
        show("quota", before)
        if buy is None:
            print("set BUY_REQUESTS=<n> to buy requests (nothing was signed or sent)")
            return

        cost = reserve_cost_micro_usdc(buy)
        print(f"buying {buy} requests for {account}: {cost / 1_000_000:.6f} USDC")
        try:
            receipt = await exchange.reserve_request_weight(buy, destination=cfg.vault)
        except ActionRejected as exc:
            print(f"rejected, nothing bought: {exc}")
            return
        except UnknownOutcome as exc:
            print(f"outcome unknown (nonce {exc.nonce}): compare the quota before and after")
            show("quota now", await info.user_rate_limit(account))
            return
        print(f"purchase acknowledged={receipt.acknowledged}")
        show("quota after", await info.user_rate_limit(account))


async def main() -> None:
    raw = os.environ.get("BUY_REQUESTS")
    buy = int(raw) if raw else None
    await run(load_config(), buy)


if __name__ == "__main__":
    asyncio.run(main())
