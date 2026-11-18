"""Signing: trading actions (EIP-712 phantom agent) and bin-ws login (EIP-191).

Everything here is a plain synchronous function. The request body returned by
:meth:`Signer.sign_action` is built by concatenating the exact action bytes the
signing image was derived from, so the bytes that are signed and the bytes that
are sent can never diverge.
"""

from __future__ import annotations

import json
import re
import secrets
from dataclasses import dataclass
from typing import Any

from coincurve import PrivateKey
from Crypto.Hash import keccak

from . import _msgpack

#: Fixed source string of the phantom agent in every environment.
DEFAULT_SOURCE = "b"
#: First line of the bin-ws login message.
LOGIN_TAG = "riverbit-ws-v2-login"

_ADDRESS_RE = re.compile(r"^0x[0-9a-fA-F]{40}$")


def keccak256(*parts: bytes) -> bytes:
    """Keccak-256 (the Ethereum variant, not NIST SHA3) of the concatenated parts."""
    digest = keccak.new(digest_bits=256)
    for part in parts:
        digest.update(part)
    return digest.digest()


def normalize_address(address: str) -> str:
    """Validate a 20-byte hex address and return it in lowercase ``0x`` form."""
    if not isinstance(address, str) or not _ADDRESS_RE.match(address):
        raise ValueError(f"invalid address: {address!r}")
    return address.lower()


def checksum_address(address: str) -> str:
    """EIP-55 mixed-case checksum form of an address."""
    plain = normalize_address(address)[2:]
    digest = keccak256(plain.encode("ascii")).hex()
    return "0x" + "".join(c.upper() if int(digest[i], 16) >= 8 else c for i, c in enumerate(plain))


@dataclass(frozen=True)
class Signature:
    """An ECDSA signature as sent in the ``signature`` member of a request."""

    r: str  # 0x + 64 hex digits
    s: str  # 0x + 64 hex digits
    v: int  # 27 or 28

    def to_bytes(self) -> bytes:
        """65 bytes ``r || s || v`` (v = 27/28)."""
        return bytes.fromhex(self.r[2:]) + bytes.fromhex(self.s[2:]) + bytes([self.v])


@dataclass(frozen=True, repr=False)
class SignedRequest:
    """A fully signed ``/exchange`` request.

    ``body`` is the exact byte string to POST to ``/exchange`` or to send as
    ``action_json``. Never rebuild it from ``action_json``: send these bytes.

    ``repr`` and ``str`` are deliberately short: they show the nonce, the action
    type and the signer address, but neither the signature nor the signed body, so
    a log line or traceback never carries a complete submittable request. The bytes
    stay available through :attr:`body`, :attr:`action_json` and :attr:`signature`.
    """

    body: bytes
    action_json: bytes
    nonce: int
    signature: Signature
    vault_address: str | None
    connection_id: bytes
    digest: bytes

    @property
    def signer_address(self) -> str | None:
        """Lowercase address recovered from the signature, or ``None`` if it cannot be."""
        try:
            from coincurve import PublicKey

            recid = self.signature.v - 27
            pub = PublicKey.from_signature_and_message(
                self.signature.to_bytes()[:64] + bytes([recid]), self.digest, hasher=None
            )
            return "0x" + keccak256(pub.format(compressed=False)[1:])[-20:].hex()
        except Exception:
            return None

    def __repr__(self) -> str:
        try:
            kind = json.loads(self.action_json).get("type")
        except (ValueError, AttributeError):
            kind = None
        parts = [f"nonce={self.nonce}", f"action={kind!r}", f"signer={self.signer_address}"]
        if self.vault_address is not None:
            parts.append(f"vault={self.vault_address}")
        return f"SignedRequest({', '.join(parts)})"

    __str__ = __repr__


def _no_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate key in action JSON: {key!r}")
        result[key] = value
    return result


def action_msgpack(action_json: bytes) -> bytes:
    """MessagePack image of an action JSON document, in received key order."""
    obj = json.loads(action_json, object_pairs_hook=_no_duplicate_keys)
    return _msgpack.packb(obj)


def connection_id(action_json: bytes, nonce: int, vault_address: str | None = None) -> bytes:
    """``keccak256(msgpack(action) || nonce (8 bytes, big endian) || tail)``.

    The tail is ``0x00`` without a vault, or ``0x01`` plus the 20-byte vault address.
    """
    if not 0 <= nonce < 1 << 64:
        raise ValueError("nonce must fit in an unsigned 64-bit integer")
    if vault_address is None:
        tail = b"\x00"
    else:
        tail = b"\x01" + bytes.fromhex(normalize_address(vault_address)[2:])
    return keccak256(action_msgpack(action_json), nonce.to_bytes(8, "big"), tail)


def _domain_separator(chain_id: int) -> bytes:
    return keccak256(
        keccak256(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
        ),
        keccak256(b"Exchange"),
        keccak256(b"1"),
        chain_id.to_bytes(32, "big"),
        bytes(32),
    )


def signing_digest(connection_id_: bytes, chain_id: int, source: str = DEFAULT_SOURCE) -> bytes:
    """EIP-712 digest of ``Agent(string source,bytes32 connectionId)``."""
    agent = keccak256(
        keccak256(b"Agent(string source,bytes32 connectionId)"),
        keccak256(source.encode("utf-8")),
        connection_id_,
    )
    return keccak256(b"\x19\x01", _domain_separator(chain_id), agent)


def login_message(host: str, timestamp_ms: int, nonce: bytes) -> str:
    """The four-line text signed for a bin-ws login, with no trailing newline."""
    return f"{LOGIN_TAG}\nhost:{host}\ntimestamp:{timestamp_ms}\nnonce:{nonce.hex()}"


def personal_hash(message: bytes) -> bytes:
    """EIP-191 version 0x45 digest of ``message``."""
    return keccak256(b"\x19Ethereum Signed Message:\n" + str(len(message)).encode("ascii"), message)


class Signer:
    """Holds one secp256k1 key and signs actions and bin-ws logins with it.

    The key is never exposed: ``repr`` and ``str`` show only the address.
    """

    def __init__(self, private_key: str | bytes) -> None:
        raw = (
            bytes.fromhex(private_key.removeprefix("0x"))
            if isinstance(private_key, str)
            else private_key
        )
        if len(raw) != 32:
            raise ValueError("private key must be 32 bytes")
        self._key = PrivateKey(raw)
        pub = self._key.public_key.format(compressed=False)[1:]
        self._address = "0x" + keccak256(pub)[-20:].hex()

    def __repr__(self) -> str:
        return f"Signer(address={self.address})"

    @property
    def address(self) -> str:
        """EIP-55 checksummed address of the signing key."""
        return checksum_address(self._address)

    @property
    def address_lower(self) -> str:
        """Lowercase ``0x`` address, the form used in requests and comparisons."""
        return self._address

    def _sign_hash(self, digest: bytes) -> Signature:
        raw = self._key.sign_recoverable(digest, hasher=None)
        return Signature(r="0x" + raw[:32].hex(), s="0x" + raw[32:64].hex(), v=raw[64] + 27)

    def sign_action(
        self,
        action_json: bytes,
        nonce: int,
        *,
        chain_id: int,
        vault_address: str | None = None,
        source: str = DEFAULT_SOURCE,
    ) -> SignedRequest:
        """Sign ``action_json`` and return the request ready to send.

        ``chain_id`` is ``nextAgentChainId`` from the ``chainIdentity`` query.
        """
        vault = normalize_address(vault_address) if vault_address is not None else None
        conn = connection_id(action_json, nonce, vault)
        digest = signing_digest(conn, chain_id, source)
        sig = self._sign_hash(digest)
        body = (
            b'{"action":'
            + action_json
            + b',"nonce":'
            + str(nonce).encode("ascii")
            + b',"signature":{"r":"'
            + sig.r.encode("ascii")
            + b'","s":"'
            + sig.s.encode("ascii")
            + b'","v":'
            + str(sig.v).encode("ascii")
            + b"}"
        )
        if vault is not None:
            body += b',"vaultAddress":"' + vault.encode("ascii") + b'"'
        body += b"}"
        return SignedRequest(body, bytes(action_json), nonce, sig, vault, conn, digest)

    def sign_login(
        self, host: str, timestamp_ms: int, nonce: bytes | None = None
    ) -> LoginSignature:
        """Sign a bin-ws login. ``nonce`` defaults to 16 random bytes."""
        if not host:
            raise ValueError("login host is empty")
        nonce = secrets.token_bytes(16) if nonce is None else nonce
        message = login_message(host, timestamp_ms, nonce)
        sig = self._sign_hash(personal_hash(message.encode("utf-8")))
        return LoginSignature(timestamp_ms, nonce, sig.to_bytes(), message)


@dataclass(frozen=True)
class LoginSignature:
    """The signed parts of a bin-ws login."""

    timestamp_ms: int
    nonce: bytes
    signature: bytes  # 65 bytes r || s || v with v = 27/28
    message: str


def recover_login_signer(host: str, timestamp_ms: int, nonce: bytes, signature: bytes) -> str:
    """Recover the lowercase address that signed a login (v may be 27/28 or 0/1)."""
    from coincurve import PublicKey

    if len(signature) != 65:
        raise ValueError("login signature must be 65 bytes")
    recid = signature[64] - 27 if signature[64] >= 27 else signature[64]
    digest = personal_hash(login_message(host, timestamp_ms, nonce).encode("utf-8"))
    pub = PublicKey.from_signature_and_message(signature[:64] + bytes([recid]), digest, hasher=None)
    return "0x" + keccak256(pub.format(compressed=False)[1:])[-20:].hex()
