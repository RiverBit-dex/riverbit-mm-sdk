"""Stream the incremental book and print the best bid and ask.

    RIVERBIT_KEY_FILE=./key python examples/stream_market_data.py BTC

Every bin-ws connection must log in, so even a market-data-only client needs an
API wallet. The session reconnects, logs in again and resubscribes by itself;
this example resets its local book on every (re)connection and resynchronises
when a frame cannot be applied.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib

from _common import Config, load_config
from riverbit_mm import (
    BinWsSession,
    Book,
    BookError,
    Connected,
    Disconnected,
    ServerFrame,
    format_fixed,
    subscriptions,
)
from riverbit_mm import frames as fr


async def stream(cfg: Config, coin: str, max_frames: int | None = None) -> None:
    book = Book(coin)
    seen = 0
    async with BinWsSession(cfg.ws_url, cfg.login_host, cfg.signer) as session:
        print(f"logged in: master={session.user} signer={session.signer_address}")
        await session.subscribe(subscriptions.l2(coin))
        await session.subscribe(subscriptions.trades(coin))
        async for item in session.frames():
            if isinstance(item, Connected):
                book.reset()  # the resubscription delivers a fresh snapshot
                print("connected" + (" (reconnect)" if item.reconnect else ""))
                continue
            if isinstance(item, Disconnected):
                print(f"disconnected: {item.error!r}")
                continue
            assert isinstance(item, ServerFrame)
            body = item.body
            try:
                applied = book.apply(body)  # typed l2 frames and the JSON fallback
            except BookError as exc:
                print(f"book out of sync ({exc}); resubscribing")
                book.reset()
                await session.unsubscribe(subscriptions.l2(coin))
                await session.subscribe(subscriptions.l2(coin))
                continue
            if applied:
                bid, ask = book.best()
                print(
                    f"{coin} bid {bid.px_text if bid else '-'} x {bid.sz_text if bid else '-'}"
                    f" | ask {ask.px_text if ask else '-'} x {ask.sz_text if ask else '-'}"
                )
            elif isinstance(body, fr.Trades):
                for trade in body.trades:
                    side = "buy" if trade.buy else "sell"
                    px = format_fixed(trade.px, body.px_decimals)
                    sz = format_fixed(trade.sz, body.sz_decimals)
                    print(f"trade {side} {sz} @ {px}")
            seen += 1
            if max_frames is not None and seen >= max_frames:
                return


async def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0] if __doc__ else None)
    parser.add_argument("coin", nargs="?", default="BTC")
    parser.add_argument("--max-frames", type=int, default=None, help="stop after N frames")
    args = parser.parse_args(argv)
    await stream(load_config(), args.coin, args.max_frames)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
