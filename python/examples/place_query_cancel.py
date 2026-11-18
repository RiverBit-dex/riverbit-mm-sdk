"""Place one post-only order, query it, cancel it.

    RIVERBIT_KEY_FILE=./key python examples/place_query_cancel.py BTC buy 50000 0.001

The order is sent for real: pick a price far from the market to keep it resting,
and use a test account. Prices and sizes are decimal strings; keep them within
the market's ``szDecimals`` and tick size. The example uses a cloid so that every
step, including recovery after an unknown outcome, can be looked up by it.
"""

from __future__ import annotations

import argparse
import asyncio

import httpx

from _common import Config, load_config
from riverbit_mm import (
    ActionRejected,
    CancelByCloidItem,
    Cloid,
    ExchangeClient,
    InfoClient,
    Order,
    UnknownOutcome,
    wait_reconciled,
)
from riverbit_mm.reconcile import order_state


async def run(
    cfg: Config,
    coin: str,
    side: str,
    price: str,
    size: str,
    http_client: httpx.AsyncClient | None = None,
) -> None:
    async with (
        InfoClient(cfg.base_url, http_client=http_client, gateway_token=cfg.gateway_token) as info,
        ExchangeClient(
            cfg.base_url,
            cfg.signer,
            vault_address=cfg.vault,
            http_client=http_client,
            gateway_token=cfg.gateway_token,
        ) as exchange,
    ):
        # Startup queries: asset index and precision, and the account that trades.
        asset = (await info.assets())[coin]
        account = cfg.vault
        if account is None:
            role = await info.user_role(cfg.signer.address_lower)
            account = (
                role["data"]["user"] if role.get("role") == "agent" else cfg.signer.address_lower
            )
        print(
            f"{coin}: asset index {asset.index}, szDecimals {asset.sz_decimals}; account {account}"
        )

        cloid = Cloid.random()
        order = Order(asset.index, side == "buy", price, size, tif="Alo", cloid=cloid)
        try:
            receipt = await exchange.place([order])
        except ActionRejected as exc:
            print(f"rejected, nothing executed: {exc} (code {exc.code})")
            return
        except UnknownOutcome as exc:
            # May or may not have executed. Never sign the order again: reconcile it.
            print(f"outcome unknown for nonce {exc.nonce}; reconciling by cloid")
            result = await wait_reconciled(info, account, exc, timeout=30)
            print(f"resolved={result.resolved}: {[s.status for s in result.placed]}")
            if not result.resolved:
                print("still unresolved; keep this request pending and check again later")
                return
        else:
            for leg in receipt.legs:
                print(f"leg {leg.index}: {leg.kind} oid={leg.oid} error={leg.error}")
            if receipt.failed:
                print("the order was not placed")
                return

        state = await order_state(info, account, cloid)
        print(f"orderStatus by cloid: {state.status} (oid {state.oid}, height {state.height})")

        try:
            cancel = await exchange.cancel_by_cloid([CancelByCloidItem(asset.index, cloid)])
        except UnknownOutcome as exc:
            print(
                f"cancel outcome unknown (nonce {exc.nonce}); reconcile before doing anything else"
            )
            return
        print(f"cancel legs: {[leg.kind for leg in cancel.legs]}")
        final = await order_state(info, account, cloid)
        print(f"after cancel: {final.status}")


async def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument("coin")
    parser.add_argument("side", choices=("buy", "sell"))
    parser.add_argument("price", help="decimal string, e.g. 50000")
    parser.add_argument("size", help="decimal string, e.g. 0.001")
    args = parser.parse_args(argv)
    await run(load_config(), args.coin, args.side, args.price, args.size)


if __name__ == "__main__":
    asyncio.run(main())
