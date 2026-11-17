"""Reproduce every entry of testdata/vectors.json byte for byte."""

from __future__ import annotations

import json
from typing import Any

import pytest

from riverbit_mm import (
    BatchModify,
    Cancel,
    CancelByCloid,
    CancelByCloidItem,
    CancelItem,
    Modify,
    ModifyItem,
    Order,
    Place,
    Signer,
)
from riverbit_mm.signing import (
    action_msgpack,
    connection_id,
    recover_login_signer,
    signing_digest,
)

_ROOT = __import__("pathlib").Path(__file__).resolve().parents[2] / "testdata" / "vectors.json"
_VECTORS = json.loads(_ROOT.read_text())


def test_test_key_address() -> None:
    signer = Signer(_VECTORS["test_key"]["private_key"])
    assert signer.address == _VECTORS["test_key"]["address"]


def test_signer_repr_hides_key() -> None:
    signer = Signer(_VECTORS["test_key"]["private_key"])
    assert "0101" not in repr(signer)
    assert signer.address in repr(signer)


@pytest.mark.parametrize("vec", _VECTORS["login"], ids=lambda v: v["name"])
def test_login_vector(vec: dict[str, Any]) -> None:
    signer = Signer(_VECTORS["test_key"]["private_key"])
    nonce = bytes.fromhex(vec["nonce_hex"])
    signed = signer.sign_login(vec["host"], vec["timestamp_ms"], nonce)
    assert signed.message == vec["message"]
    assert signed.signature.hex() == vec["signature_hex"]
    assert signed.nonce == nonce and len(signed.signature) == 65
    recovered = recover_login_signer(vec["host"], vec["timestamp_ms"], nonce, signed.signature)
    assert recovered == signer.address_lower


@pytest.mark.parametrize("vec", _VECTORS["actions"], ids=lambda v: v["name"])
def test_action_vector(vec: dict[str, Any]) -> None:
    signer = Signer(_VECTORS["test_key"]["private_key"])
    action_json = vec["action_json"].encode("utf-8")
    vault = vec["vault_address"]

    assert action_msgpack(action_json).hex() == vec["msgpack_hex"]
    conn = connection_id(action_json, vec["nonce"], vault)
    assert "0x" + conn.hex() == vec["connection_id"]
    assert "0x" + signing_digest(conn, vec["chain_id"], vec["source"]).hex() == vec["digest"]

    request = signer.sign_action(
        action_json,
        vec["nonce"],
        chain_id=vec["chain_id"],
        vault_address=vault,
        source=vec["source"],
    )
    assert request.signature.r == vec["signature"]["r"]
    assert request.signature.s == vec["signature"]["s"]
    assert request.signature.v == vec["signature"]["v"]

    # The bytes that are signed are the bytes that are sent.
    assert request.action_json == action_json
    assert request.body.startswith(b'{"action":' + action_json + b',"nonce":')
    parsed = json.loads(request.body)
    assert parsed["nonce"] == vec["nonce"]
    assert parsed["signature"] == vec["signature"]
    if vault is None:
        assert "vaultAddress" not in parsed
    else:
        assert parsed["vaultAddress"] == vault.lower()
        assert request.body.endswith(b',"vaultAddress":"' + vault.lower().encode() + b'"}')


def test_vectors_cover_the_basics() -> None:
    names = {v["name"] for v in _VECTORS["actions"]}
    assert {"order-no-vault", "order-vault"} <= names
    assert _VECTORS["login"]


def test_duplicate_keys_are_rejected() -> None:
    with pytest.raises(ValueError, match="duplicate key"):
        action_msgpack(b'{"type":"order","type":"cancel"}')


def test_nonce_range() -> None:
    with pytest.raises(ValueError):
        connection_id(b'{"type":"cancel","cancels":[]}', -1)
    with pytest.raises(ValueError):
        connection_id(b'{"type":"cancel","cancels":[]}', 1 << 64)


# ---------------------------------------------------------------- typed actions


def _order(obj: dict[str, Any]) -> Order:
    return Order(
        asset=obj["a"],
        is_buy=obj["b"],
        price=obj["p"],
        size=obj["s"],
        reduce_only=obj["r"],
        tif=obj["t"]["limit"]["tif"],
        cloid=obj.get("c"),
    )


def _typed_action(obj: dict[str, Any]) -> Any:
    kind = obj["type"]
    if kind == "order":
        return Place([_order(o) for o in obj["orders"]], obj["grouping"])
    if kind == "cancel":
        return Cancel([CancelItem(c["a"], c["o"]) for c in obj["cancels"]])
    if kind == "cancelByCloid":
        return CancelByCloid([CancelByCloidItem(c["asset"], c["cloid"]) for c in obj["cancels"]])
    if kind == "modify":
        return Modify(obj["oid"], _order(obj["order"]))
    if kind == "batchModify":
        return BatchModify([ModifyItem(m["oid"], _order(m["order"])) for m in obj["modifies"]])
    raise AssertionError(f"vector with an action type the SDK does not build: {kind}")


@pytest.mark.parametrize("vec", _VECTORS["actions"], ids=lambda v: v["name"])
def test_typed_action_serialises_to_the_vector_bytes(vec: dict[str, Any]) -> None:
    action = _typed_action(json.loads(vec["action_json"]))
    assert action.to_json() == vec["action_json"].encode("utf-8")
