from __future__ import annotations

import json
import threading

import pytest

from riverbit_mm import (
    BatchModify,
    Cancel,
    CancelByCloid,
    CancelByCloidItem,
    CancelItem,
    Cloid,
    Modify,
    ModifyItem,
    NonceSource,
    Order,
    Place,
)
from riverbit_mm._msgpack import packb

CLOID = "0x00000000000000000000000000000001"


def test_order_field_order_and_strings() -> None:
    order = Order(1, True, "80000", "0.001", cloid=Cloid.from_int(1))
    assert Place([order]).to_json() == (
        b'{"type":"order","orders":[{"a":1,"b":true,"p":"80000","s":"0.001","r":false,'
        b'"t":{"limit":{"tif":"Alo"}},"c":"' + CLOID.encode() + b'"}],"grouping":"na"}'
    )


def test_trailing_zeroes_are_preserved() -> None:
    body = Place([Order(0, False, "52.000", "1.50")]).to_json()
    assert b'"p":"52.000"' in body and b'"s":"1.50"' in body


def test_cloid_is_omitted_when_absent() -> None:
    assert b'"c"' not in Place([Order(0, True, "1", "1")]).to_json()


@pytest.mark.parametrize("bad", [100.5, 100, None, b"1"])
def test_prices_and_sizes_must_be_strings(bad: object) -> None:
    with pytest.raises(TypeError):
        Order(0, True, bad, "1")  # type: ignore[arg-type]
    with pytest.raises(TypeError):
        Order(0, True, "1", bad)  # type: ignore[arg-type]


def test_tif_validation() -> None:
    assert b'"tif":"Gtc"' in Place([Order(0, True, "1", "1", tif="Gtc")]).to_json()
    with pytest.raises(ValueError):
        Order(0, True, "1", "1", tif="gtc")


def test_cancel_actions() -> None:
    assert Cancel([CancelItem(1, 123456789)]).to_json() == (
        b'{"type":"cancel","cancels":[{"a":1,"o":123456789}]}'
    )
    assert CancelByCloid([CancelByCloidItem(1, CLOID)]).to_json() == (
        b'{"type":"cancelByCloid","cancels":[{"asset":1,"cloid":"' + CLOID.encode() + b'"}]}'
    )


def test_modify_actions() -> None:
    order = Order(1, True, "80100", "0.001", cloid=CLOID)
    assert BatchModify([ModifyItem(7, order)]).to_json() == (
        b'{"type":"batchModify","modifies":[{"oid":7,"order":{"a":1,"b":true,"p":"80100",'
        b'"s":"0.001","r":false,"t":{"limit":{"tif":"Alo"}},"c":"' + CLOID.encode() + b'"}}]}'
    )
    assert Modify(7, order).to_json().startswith(b'{"type":"modify","oid":7,"order":{"a":1')


def test_empty_actions_are_rejected() -> None:
    for build in (
        lambda: Place([]),
        lambda: Cancel([]),
        lambda: CancelByCloid([]),
        lambda: BatchModify([]),
    ):
        with pytest.raises(ValueError):
            build()


def test_cloid_forms() -> None:
    assert str(Cloid.from_int(255)) == "0x000000000000000000000000000000ff"
    assert Cloid.from_hex(CLOID) == Cloid.from_int(1)
    assert len(Cloid.random().value) == 16 and Cloid.random() != Cloid.random()
    for bad in ("0X" + "0" * 32, "0x" + "A" * 32, "0x" + "0" * 31, "0" * 34):
        with pytest.raises(ValueError):
            Cloid.from_hex(bad)
    with pytest.raises(ValueError):
        Order(0, True, "1", "1", cloid="0x" + "A" * 32)


def test_msgpack_matches_known_encodings() -> None:
    assert packb(0) == b"\x00" and packb(127) == b"\x7f"
    assert packb(128) == b"\xcc\x80" and packb(255) == b"\xcc\xff"
    assert packb(256) == b"\xcd\x01\x00" and packb(65536) == b"\xce\x00\x01\x00\x00"
    assert packb(2**32) == b"\xcf\x00\x00\x00\x01\x00\x00\x00\x00"
    assert packb(-1) == b"\xff" and packb(-33) == b"\xd0\xdf"
    assert packb(True) == b"\xc3" and packb(False) == b"\xc2" and packb(None) == b"\xc0"
    assert packb(1.5) == b"\xcb" + bytes.fromhex("3ff8000000000000")
    assert packb("a" * 32) == b"\xd9\x20" + b"a" * 32
    assert packb([1, 2]) == b"\x92\x01\x02"
    assert packb({"b": 1, "a": 2}) == b"\x82\xa1b\x01\xa1a\x02"  # insertion order, not sorted
    with pytest.raises(TypeError):
        packb({1: 2})
    with pytest.raises(OverflowError):
        packb(2**64)


def test_action_json_round_trips_in_key_order() -> None:
    action = Place([Order(2, False, "10", "3", reduce_only=True, cloid=CLOID)])
    assert list(json.loads(action.to_json())) == ["type", "orders", "grouping"]


# ---------------------------------------------------------------- nonce


def test_nonce_is_strictly_increasing_with_a_stalled_clock() -> None:
    source = NonceSource(clock_ms=lambda: 1000)
    assert [source.next() for _ in range(3)] == [1000, 1001, 1002]


def test_nonce_survives_a_clock_going_backwards() -> None:
    times = iter([5000, 4000, 4000, 6000])
    source = NonceSource(clock_ms=lambda: next(times))
    assert [source.next() for _ in range(4)] == [5000, 5001, 5002, 6000]


def test_nonce_observe_raises_the_floor() -> None:
    source = NonceSource(clock_ms=lambda: 10)
    source.observe(500)
    assert source.next() == 501


def test_nonce_is_unique_under_concurrency() -> None:
    source = NonceSource()
    results: list[list[int]] = []

    def worker() -> None:
        results.append([source.next() for _ in range(2000)])

    threads = [threading.Thread(target=worker) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    flat = [n for chunk in results for n in chunk]
    assert len(set(flat)) == len(flat) == 16000
    for chunk in results:  # each thread sees an increasing sequence
        assert chunk == sorted(chunk)


# ---------------------------------------------------------------- reserveRequestWeight


def test_reserve_request_weight_key_order_and_integer_weight() -> None:
    from riverbit_mm import ReserveRequestWeight, reserve_cost_micro_usdc

    assert ReserveRequestWeight(1000).to_json() == b'{"type":"reserveRequestWeight","weight":1000}'
    upper = "0x" + "AB" * 20
    assert ReserveRequestWeight(7, upper).to_json() == (
        b'{"type":"reserveRequestWeight","weight":7,"destination":"0x' + b"ab" * 20 + b'"}'
    )
    assert reserve_cost_micro_usdc(1000) == 500_000
    assert ReserveRequestWeight(3).cost_micro_usdc == 1500


@pytest.mark.parametrize("bad", [0, -1, 1_000_000_001, True, 1.5, "10"])
def test_reserve_request_weight_is_validated_before_signing(bad: object) -> None:
    from riverbit_mm import ReserveRequestWeight

    with pytest.raises(ValueError):
        ReserveRequestWeight(bad)  # type: ignore[arg-type]


def test_reserve_request_weight_bounds_and_destination() -> None:
    from riverbit_mm import ReserveRequestWeight

    assert ReserveRequestWeight(1).weight == 1
    assert ReserveRequestWeight(1_000_000_000).weight == 1_000_000_000
    with pytest.raises(ValueError):
        ReserveRequestWeight(1, "0x1234")
