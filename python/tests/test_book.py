from __future__ import annotations

from decimal import Decimal

import pytest

from riverbit_mm import Book, BookError
from riverbit_mm import frames as fr


def snapshot(**kw: object) -> fr.L2Delta:
    base: dict[str, object] = {
        "coin": "BTC",
        "time": 1,
        "px_decimals": 1,
        "sz_decimals": 2,
        "bid_px": (1000, 990, 980),
        "bid_sz": (100, 200, 300),
        "ask_px": (1010, 1020),
        "ask_sz": (50, 60),
    }
    base.update(kw)
    return fr.L2Delta(snapshot=fr.L2Book(**base))  # type: ignore[arg-type]


def update(**kw: object) -> fr.L2Delta:
    base: dict[str, object] = {"coin": "BTC", "time": 2, "px_decimals": 1, "sz_decimals": 2}
    base.update(kw)
    return fr.L2Delta(update=fr.L2DeltaUpdate(**base))  # type: ignore[arg-type]


def px(book: Book) -> tuple[list[str], list[str]]:
    return [x.px_text for x in book.bids], [x.px_text for x in book.asks]


def test_snapshot_then_update() -> None:
    book = Book("BTC")
    assert book.apply(snapshot())
    assert px(book) == (["100.0", "99.0", "98.0"], ["101.0", "102.0"])
    best_bid, best_ask = book.best()
    assert best_bid is not None and best_ask is not None
    assert (best_bid.px_text, best_bid.sz_text) == ("100.0", "1.00")
    assert best_ask.px == Decimal("101.0")

    # remove bid index 2 and ask index 0 of the PREVIOUS book as one set, change one, add one
    assert book.apply(
        update(
            bid_px=(1000, 995),
            bid_sz=(150, 10),
            ask_px=(1015,),
            ask_sz=(7,),
            bid_removed=(2,),
            ask_removed=(0,),
            spread=5,
        )
    )
    assert px(book) == (["100.0", "99.5", "99.0"], ["101.5", "102.0"])
    assert book.bids[0].sz_text == "1.50"
    assert book.last_spread == Decimal("0.5")
    assert book.time == 2


def test_removed_indices_refer_to_the_previous_book() -> None:
    book = Book("BTC")
    book.apply(snapshot())
    # The inserted level sorts to the front, but index 0 still means the old best bid.
    book.apply(update(bid_px=(1005,), bid_sz=(9,), bid_removed=(0,)))
    assert px(book)[0] == ["100.5", "99.0", "98.0"]


def test_snapshot_replaces_everything_at_any_time() -> None:
    book = Book("BTC")
    book.apply(snapshot())
    book.apply(snapshot(px_decimals=2, bid_px=(10000,), bid_sz=(1,), ask_px=(), ask_sz=()))
    assert px(book) == (["100.00"], []) and book.px_decimals == 2
    book.apply(update(px_decimals=2, bid_px=(9999,), bid_sz=(5,)))
    assert px(book)[0] == ["100.00", "99.99"]


def test_update_before_snapshot() -> None:
    with pytest.raises(BookError, match="before any snapshot"):
        Book("BTC").apply(update())


def test_decimals_change_requires_resubscribe() -> None:
    book = Book("BTC")
    book.apply(snapshot())
    with pytest.raises(BookError, match="decimals"):
        book.apply(update(px_decimals=2))


def test_invalid_update_leaves_book_untouched() -> None:
    book = Book("BTC")
    book.apply(snapshot())
    before = px(book)
    with pytest.raises(BookError, match="outside the previous book"):
        book.apply(update(bid_px=(1001,), bid_sz=(1,), bid_removed=(9,)))
    assert px(book) == before and book.time == 1
    assert book.time == 1


def test_wrong_coin_and_unordered_snapshot() -> None:
    with pytest.raises(BookError):
        Book("ETH").apply(snapshot())
    with pytest.raises(BookError):
        Book("BTC").apply(snapshot(bid_px=(990, 1000), bid_sz=(1, 1)))


def test_reset_clears_state() -> None:
    book = Book("BTC")
    book.apply(snapshot())
    book.reset()
    assert not book.has_book and book.best() == (None, None) and book.coin == "BTC"


def test_other_frames_are_ignored() -> None:
    book = Book("BTC")
    assert not book.apply(fr.Bbo("BTC", 1, 1, 1))
    assert not book.apply(fr.JsonFrame(b'{"channel":"trades","data":[]}'))


def json_frame(data: str) -> fr.JsonFrame:
    return fr.JsonFrame(b'{"channel":"l2","data":' + data.encode() + b"}")


def test_json_fallback() -> None:
    book = Book("BTC")
    assert book.apply(
        json_frame(
            '{"s":{"coin":"BTC","time":5,"levels":[[{"px":"100.0","sz":"1.5"},'
            '{"px":"99.0","sz":"2"}],[{"px":"101.0","sz":"1"}]]}}'
        )
    )
    assert px(book) == (["100.0", "99.0"], ["101.0"])
    assert not book.locked
    assert book.apply(
        json_frame('{"u":{"c":"BTC","t":6,"l":[[{"p":"99.5","s":"3"}],[]],"r":[[1],[]],"s":"1.0"}}')
    )
    assert px(book) == (["100.0", "99.5"], ["101.0"])
    assert book.last_spread == Decimal("1.0")
    # A typed update cannot be applied to a book built from a JSON snapshot.
    with pytest.raises(BookError, match="JSON snapshot"):
        book.apply(update())


def test_json_fallback_errors() -> None:
    book = Book("BTC")
    with pytest.raises(BookError):
        book.apply(json_frame('{"u":{"c":"BTC","t":1,"l":[[],[]]}}'))  # no snapshot yet
    with pytest.raises(BookError):
        book.apply(json_frame('{"x":1}'))
    with pytest.raises(BookError):
        book.apply(json_frame('{"s":{"levels":[[{"px":"abc","sz":"1"}],[]]}}'))
    with pytest.raises(BookError):
        book.apply(json_frame("[1]"))
