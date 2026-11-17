//! Signing: trading actions (EIP-712 over the MessagePack image) and the
//! bin-ws login (EIP-191 `personal_sign`).
//!
//! The bytes that are signed and the bytes that are sent are the same bytes.
//! A [`SignedRequest`] is built once from the action JSON and never
//! reserialized.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use sha3::{Digest, Keccak256};

use crate::actions::{ActionKind, OrderRef};
use crate::error::Error;
use crate::msgpack;
use crate::proto::Login;

type Result<T> = std::result::Result<T, Error>;

/// A 20-byte account address. Displayed as lowercase `0x` plus 40 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address([u8; 20]);

impl Address {
    /// Wraps 20 raw bytes.
    pub const fn from_bytes(bytes: [u8; 20]) -> Self {
        Address(bytes)
    }

    /// The 20 raw bytes.
    pub const fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({self})")
    }
}

impl FromStr for Address {
    type Err = Error;

    /// Accepts `0x` plus 40 hex digits in any case (checksums are not checked).
    fn from_str(text: &str) -> Result<Self> {
        let invalid = || Error::InvalidInput(format!("invalid address {text:?}"));
        let digits = text.strip_prefix("0x").ok_or_else(invalid)?;
        let mut bytes = [0u8; 20];
        if digits.len() != 40 {
            return Err(invalid());
        }
        hex::decode_to_slice(digits, &mut bytes).map_err(|_| invalid())?;
        Ok(Address(bytes))
    }
}

/// The EIP-712 domain parameters of trading signatures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigningDomain {
    /// `chainIdentity.nextAgentChainId`. Never hard-code it.
    pub chain_id: u64,
    /// The agent `source` string; `"b"` in all environments today.
    pub source: String,
}

impl SigningDomain {
    /// A domain with source `"b"`.
    pub fn new(chain_id: u64) -> Self {
        SigningDomain {
            chain_id,
            source: "b".to_string(),
        }
    }
}

/// An ECDSA trading signature as sent in the request: `r`, `s` and `v` (27 or 28).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionSignature {
    /// 32-byte `r`.
    pub r: [u8; 32],
    /// 32-byte `s`.
    pub s: [u8; 32],
    /// 27 or 28.
    pub v: u8,
}

impl ActionSignature {
    /// `r` as `0x` plus 64 hex digits.
    pub fn r_hex(&self) -> String {
        format!("0x{}", hex::encode(self.r))
    }

    /// `s` as `0x` plus 64 hex digits (leading zeros kept; the node would
    /// also accept them stripped).
    pub fn s_hex(&self) -> String {
        format!("0x{}", hex::encode(self.s))
    }
}

/// Holds a private key and signs. The key is never printed or exposed.
#[derive(Clone)]
pub struct Signer {
    key: Arc<SigningKey>,
    address: Address,
}

impl fmt::Debug for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Signer")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Signer {
    /// From a hex private key, with or without `0x`.
    pub fn from_hex(private_key: &str) -> Result<Self> {
        let digits = private_key.trim().trim_start_matches("0x");
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(digits, &mut bytes)
            .map_err(|_| Error::InvalidInput("private key must be 32 bytes of hex".into()))?;
        Self::from_bytes(&bytes)
    }

    /// From 32 raw key bytes.
    pub fn from_bytes(private_key: &[u8; 32]) -> Result<Self> {
        let key = SigningKey::from_bytes(private_key.into())
            .map_err(|_| Error::InvalidInput("invalid private key".into()))?;
        let address = address_of(key.verifying_key());
        Ok(Signer {
            key: Arc::new(key),
            address,
        })
    }

    /// The signer's address.
    pub fn address(&self) -> Address {
        self.address
    }

    /// EIP-191 `personal_sign`: 65 bytes `r || s || v` with `v` 27 or 28.
    pub fn personal_sign(&self, message: &[u8]) -> Result<[u8; 65]> {
        let (r, s, v) = self.sign_digest(&personal_hash(message))?;
        let mut out = [0u8; 65];
        out[..32].copy_from_slice(&r);
        out[32..64].copy_from_slice(&s);
        out[64] = v;
        Ok(out)
    }

    /// Builds the signed bin-ws login for `host` at `timestamp_ms` with the
    /// given 16-byte nonce.
    pub fn sign_login(&self, host: &str, timestamp_ms: u64, nonce: [u8; 16]) -> Result<Login> {
        if host.is_empty() {
            return Err(Error::InvalidInput("login host is empty".into()));
        }
        let signature = self.personal_sign(login_message(host, timestamp_ms, &nonce).as_bytes())?;
        Ok(Login {
            timestamp_ms,
            nonce: nonce.to_vec(),
            signature: signature.to_vec(),
        })
    }

    /// Signs `action_json` (the exact bytes that will be sent) with `nonce`.
    /// `vault` selects the vault tail of the `connectionId`.
    pub fn sign_action(
        &self,
        action_json: &str,
        nonce: u64,
        vault: Option<&Address>,
        domain: &SigningDomain,
    ) -> Result<ActionSignature> {
        let packed = action_msgpack(action_json)?;
        let connection_id = connection_id(&packed, nonce, vault);
        let digest = action_digest(&connection_id, domain);
        let (r, s, v) = self.sign_digest(&digest)?;
        Ok(ActionSignature { r, s, v })
    }

    /// Signs an action and assembles the request body that is sent as is:
    /// `{"action":<action_json>,"nonce":N,"signature":{...}[,"vaultAddress":"0x.."]}`.
    pub fn sign_request(
        &self,
        action_json: String,
        nonce: u64,
        vault: Option<&Address>,
        domain: &SigningDomain,
        info: ActionInfo,
    ) -> Result<SignedRequest> {
        let signature = self.sign_action(&action_json, nonce, vault, domain)?;
        let mut body = String::with_capacity(action_json.len() + 220);
        body.push_str("{\"action\":");
        body.push_str(&action_json);
        body.push_str(&format!(
            ",\"nonce\":{nonce},\"signature\":{{\"r\":\"{}\",\"s\":\"{}\",\"v\":{}}}",
            signature.r_hex(),
            signature.s_hex(),
            signature.v
        ));
        if let Some(vault) = vault {
            body.push_str(&format!(",\"vaultAddress\":\"{vault}\""));
        }
        body.push('}');
        Ok(SignedRequest {
            body: body.into_bytes(),
            nonce,
            signer: self.address,
            vault: vault.copied(),
            info,
        })
    }

    fn sign_digest(&self, digest: &[u8; 32]) -> Result<([u8; 32], [u8; 32], u8)> {
        let (sig, recid) = self
            .key
            .sign_prehash_recoverable(digest)
            .map_err(|e| Error::InvalidInput(format!("signing failed: {e}")))?;
        let (r, s) = sig.split_bytes();
        Ok((r.into(), s.into(), 27 + recid.to_byte()))
    }
}

/// What an action is and which orders it refers to. Kept with the signed
/// request so a receipt can be checked and an unknown outcome reconciled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionInfo {
    /// The action kind.
    pub kind: ActionKind,
    /// Expected number of `statuses[]` entries, if any.
    pub expected_legs: Option<usize>,
    /// Orders the action refers to.
    pub targets: Vec<OrderRef>,
}

/// An action that has been signed and serialized: the request body, ready to
/// send as the body of `POST /exchange` or as `action_json`.
#[derive(Clone, PartialEq, Eq)]
pub struct SignedRequest {
    body: Vec<u8>,
    nonce: u64,
    signer: Address,
    vault: Option<Address>,
    info: ActionInfo,
}

impl SignedRequest {
    /// The exact bytes to send.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The nonce.
    pub fn nonce(&self) -> u64 {
        self.nonce
    }

    /// The signing address.
    pub fn signer(&self) -> Address {
        self.signer
    }

    /// The vault the request trades for, if any.
    pub fn vault(&self) -> Option<Address> {
        self.vault
    }

    /// The action kind.
    pub fn kind(&self) -> ActionKind {
        self.info.kind
    }

    /// Expected number of `statuses[]` entries.
    pub fn expected_legs(&self) -> Option<usize> {
        self.info.expected_legs
    }

    /// The orders this request refers to (oids and cloids), for reconciliation.
    pub fn targets(&self) -> &[OrderRef] {
        &self.info.targets
    }
}

impl fmt::Debug for SignedRequest {
    // The body is replayable; keep it out of logs by default.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignedRequest")
            .field("kind", &self.info.kind)
            .field("nonce", &self.nonce)
            .field("signer", &self.signer)
            .field("vault", &self.vault)
            .field("body_len", &self.body.len())
            .finish()
    }
}

/// The MessagePack image of an action JSON string, in received key order.
pub fn action_msgpack(action_json: &str) -> Result<Vec<u8>> {
    let value: serde_json::Value = serde_json::from_str(action_json)
        .map_err(|e| Error::InvalidInput(format!("action is not valid JSON: {e}")))?;
    msgpack::encode(&value)
}

/// `keccak256(msgpack || nonce as 8-byte big endian || tail)`, where the tail
/// is `0x00` without a vault and `0x01` plus the 20-byte vault with one.
pub fn connection_id(msgpack: &[u8], nonce: u64, vault: Option<&Address>) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(msgpack);
    hasher.update(nonce.to_be_bytes());
    match vault {
        None => hasher.update([0u8]),
        Some(v) => {
            hasher.update([1u8]);
            hasher.update(v.0);
        }
    }
    hasher.finalize().into()
}

/// The EIP-712 digest of `Agent(string source,bytes32 connectionId)` under the
/// `Exchange` domain.
pub fn action_digest(connection_id: &[u8; 32], domain: &SigningDomain) -> [u8; 32] {
    let domain_sep = keccak(&[
        &keccak(&[
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
        ]),
        &keccak(&[b"Exchange"]),
        &keccak(&[b"1"]),
        &word(domain.chain_id),
        &[0u8; 32],
    ]);
    let agent = keccak(&[
        &keccak(&[b"Agent(string source,bytes32 connectionId)"]),
        &keccak(&[domain.source.as_bytes()]),
        connection_id,
    ]);
    keccak(&[&[0x19, 0x01], &domain_sep, &agent])
}

/// The four-line login text that is signed, without a trailing newline.
pub fn login_message(host: &str, timestamp_ms: u64, nonce: &[u8]) -> String {
    format!(
        "riverbit-ws-v2-login\nhost:{host}\ntimestamp:{timestamp_ms}\nnonce:{}",
        hex::encode(nonce)
    )
}

/// The EIP-191 version 0x45 digest of `message`.
pub fn personal_hash(message: &[u8]) -> [u8; 32] {
    keccak(&[
        b"\x19Ethereum Signed Message:\n",
        message.len().to_string().as_bytes(),
        message,
    ])
}

/// Recovers the address that produced `signature` (65 bytes `r || s || v`,
/// `v` 27/28 or 0/1) over the EIP-191 digest of `message`.
pub fn recover_personal(message: &[u8], signature: &[u8]) -> Result<Address> {
    if signature.len() != 65 {
        return Err(Error::InvalidInput(format!(
            "signature is {} bytes, want 65",
            signature.len()
        )));
    }
    let v = if signature[64] >= 27 {
        signature[64] - 27
    } else {
        signature[64]
    };
    let recid = RecoveryId::from_byte(v)
        .ok_or_else(|| Error::InvalidInput("invalid recovery id".into()))?;
    let sig = Signature::from_slice(&signature[..64])
        .map_err(|e| Error::InvalidInput(format!("invalid signature: {e}")))?;
    let key = VerifyingKey::recover_from_prehash(&personal_hash(message), &sig, recid)
        .map_err(|e| Error::InvalidInput(format!("cannot recover signer: {e}")))?;
    Ok(address_of(&key))
}

fn address_of(key: &VerifyingKey) -> Address {
    let point = key.to_encoded_point(false);
    let hash = keccak(&[&point.as_bytes()[1..]]);
    let mut out = [0u8; 20];
    out.copy_from_slice(&hash[12..]);
    Address(out)
}

fn keccak(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn word(n: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&n.to_be_bytes());
    out
}
