"""Keep orders in sync across bin-ws reconnects.

    RIVERBIT_KEY_FILE=./key python examples/reconcile_after_reconnect.py BTC

After every reconnect the session has already logged in again and resubscribed.
What is left to the application, and what this example does on each
``Connected(reconnect=True)``:

1. buffer pushes that arrive while recovering,
2. read ``openOrders`` (REST) to rebuild the order list,
3. reconcile every request whose outcome is still unknown, by oid or cloid,
4. only then process the buffered pushes.

A cancel request stays pending even when its target is already terminal: order
state cannot show that the request itself has finished, and a cloid must not be
reused while any request naming it is unresolved.

``pending`` stands in for your own journal of requests that were sent but not
confirmed. The SDK does not persist it for you (see the README); a production
maker writes it to durable storage before sending.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib

import httpx

from _common import Config, load_config
from riverbit_mm import (
    BinWsSession,
    Connected,
    Disconnected,
    InfoClient,
    JsonFrame,
    ServerFrame,
    SignedRequest,
    subscriptions,
    wait_reconciled,
)


async def recover(info: InfoClient, account: str, pending: dict[int, SignedRequest]) -> None:
    open_orders = await info.open_orders(account)
    print(f"open orders after reconnect: {len(open_orders)}")
    for nonce, request in list(pending.items()):
        result = await wait_reconciled(info, account, request, timeout=30)
        if result.resolved:
            print(f"request {nonce} resolved: {[s.status for s in result.placed]}")
            del pending[nonce]
        elif result.targets_settled:
            # A cancel whose target is terminal is still pending: the target may have ended
            # for another reason while this request is in flight, and a late cancelByCloid
            # would cancel a new order that reused the cloid. Keep it, and keep its cloids
            # out of use (the best rule is never to reuse a cloid).
            print(f"request {nonce} target is terminal, request unconfirmed: keep it pending")
            print(f"  cloids on hold: {list(result.hold_cloids)}")
        else:
            # Unresolved is not "failed": keep it pending and keep conflicting writes on hold.
            print(f"request {nonce} still unresolved: {result.unresolved}")


async def run(
    cfg: Config,
    coin: str,
    max_events: int | None = None,
    http_client: httpx.AsyncClient | None = None,
) -> None:
    pending: dict[int, SignedRequest] = {}  # filled by your own order-entry code
    buffered: list[ServerFrame] = []
    recovering = False
    events = 0
    async with (
        InfoClient(cfg.base_url, http_client=http_client) as info,
        BinWsSession(cfg.ws_url, cfg.login_host, cfg.signer) as session,
    ):
        account = cfg.vault or session.user
        await session.subscribe(subscriptions.order_updates(account))
        await session.subscribe(subscriptions.user_fills(account))
        await session.subscribe(subscriptions.l2(coin))
        async for item in session.frames():
            if isinstance(item, Disconnected):
                print(f"disconnected: {item.error!r}; reconnecting={item.will_reconnect}")
                recovering = True
            elif isinstance(item, Connected):
                if item.reconnect or recovering:
                    recovering = True
                    await recover(info, account, pending)
                    recovering = False
                    for frame in buffered:
                        handle(frame)
                    buffered.clear()
            elif recovering:
                buffered.append(item)
            else:
                handle(item)
            events += 1
            if max_events is not None and events >= max_events:
                return


def handle(frame: ServerFrame) -> None:
    body = frame.body
    if isinstance(body, JsonFrame) and body.channel in ("orderUpdates", "userFills"):
        print(f"{body.channel}: {str(body.data)[:200]}")


async def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument("coin", nargs="?", default="BTC")
    args = parser.parse_args(argv)
    await run(load_config(), args.coin)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
