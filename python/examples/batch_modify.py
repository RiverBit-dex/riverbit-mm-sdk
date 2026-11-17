"""Place two orders, move both with one batchModify, cancel them.

    RIVERBIT_KEY_FILE=./key python examples/batch_modify.py BTC 50000 51000 0.001

``batchModify`` is preferred over ``modify``, even for one order, because each
leg of its receipt carries the new oid. Pass the original cloid in the
replacement to keep it. Partial success is not rolled back: look at every leg.
"""

from __future__ import annotations

import argparse
import asyncio

import httpx

from _common import Config, load_config
from riverbit_mm import (
    BatchModify,
    CancelItem,
    Cloid,
    ExchangeClient,
    InfoClient,
    ModifyItem,
    Order,
    Place,
    UnknownOutcome,
)


async def run(
    cfg: Config,
    coin: str,
    buy_price: str,
    sell_price: str,
    size: str,
    http_client: httpx.AsyncClient | None = None,
) -> None:
    async with (
        InfoClient(cfg.base_url, http_client=http_client) as info,
        ExchangeClient(
            cfg.base_url, cfg.signer, vault_address=cfg.vault, http_client=http_client
        ) as exchange,
    ):
        asset = (await info.assets())[coin]
        buy_cloid, sell_cloid = Cloid.random(), Cloid.random()
        placed = await exchange.post(
            Place(
                [
                    Order(asset.index, True, buy_price, size, cloid=buy_cloid),
                    Order(asset.index, False, sell_price, size, cloid=sell_cloid),
                ]
            )
        )
        oids = [leg.oid for leg in placed.legs]
        print(f"placed: {[leg.kind for leg in placed.legs]} oids={oids}")
        live = {leg.index: leg.oid for leg in placed.legs if leg.ok and leg.oid}
        if len(live) != 2:
            print("not every leg is resting; stopping (the legs that rested stay live)")
            return

        # Move each order by one unit in the last decimal place of its price string.
        new_buy = str(int(buy_price) + 1) if buy_price.isdigit() else buy_price
        new_sell = str(int(sell_price) + 1) if sell_price.isdigit() else sell_price
        try:
            modified = await exchange.post(
                BatchModify(
                    [
                        ModifyItem(
                            live[0], Order(asset.index, True, new_buy, size, cloid=buy_cloid)
                        ),
                        ModifyItem(
                            live[1], Order(asset.index, False, new_sell, size, cloid=sell_cloid)
                        ),
                    ]
                )
            )
        except UnknownOutcome as exc:
            print(f"batchModify outcome unknown (nonce {exc.nonce}); reconcile, do not resend")
            return
        for old, leg in zip(live.values(), modified.legs, strict=False):
            if leg.ok:
                print(f"oid {old} -> {leg.oid}")
            else:
                print(f"oid {old} not replaced: {leg.error}")
        new_oids = [leg.oid for leg in modified.legs if leg.ok and leg.oid]
        old_oids = [
            old for old, leg in zip(live.values(), modified.legs, strict=False) if not leg.ok
        ]
        remaining = new_oids + old_oids
        if remaining:
            cancel = await exchange.cancel([CancelItem(asset.index, oid) for oid in remaining])
            print(f"cancel legs: {[leg.kind for leg in cancel.legs]}")


async def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument("coin")
    parser.add_argument("buy_price")
    parser.add_argument("sell_price")
    parser.add_argument("size")
    args = parser.parse_args(argv)
    await run(load_config(), args.coin, args.buy_price, args.sell_price, args.size)


if __name__ == "__main__":
    asyncio.run(main())
