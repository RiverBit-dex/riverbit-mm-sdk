//! Client order IDs.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize, Serializer};

use crate::error::Error;

/// A 128-bit client order ID.
///
/// On the wire (JSON and the signed MessagePack image) it is the lowercase
/// string `0x` followed by 32 hex digits. The same string is signed and sent.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cloid([u8; 16]);

impl Cloid {
    /// Wraps 16 raw bytes.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Cloid(bytes)
    }

    /// Builds a cloid from an integer, big endian. Handy for counters.
    pub const fn from_u128(value: u128) -> Self {
        Cloid(value.to_be_bytes())
    }

    /// A random cloid from the operating system's random number generator.
    pub fn random() -> Result<Self, Error> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| Error::Transport(format!("random cloid: {e}")))?;
        Ok(Cloid(bytes))
    }

    /// The 16 raw bytes.
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for Cloid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl fmt::Debug for Cloid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cloid({self})")
    }
}

impl FromStr for Cloid {
    type Err = Error;

    /// Accepts `0x` plus exactly 32 hex digits. Uppercase digits are
    /// accepted here and normalized to lowercase; the exchange itself
    /// requires lowercase in `orderStatus` queries.
    fn from_str(text: &str) -> Result<Self, Error> {
        let invalid = || Error::InvalidInput("cloid must be 0x followed by 32 hex digits".into());
        let digits = text.strip_prefix("0x").ok_or_else(invalid)?;
        if digits.len() != 32 {
            return Err(invalid());
        }
        let mut bytes = [0u8; 16];
        hex::decode_to_slice(digits, &mut bytes).map_err(|_| invalid())?;
        Ok(Cloid(bytes))
    }
}

impl Serialize for Cloid {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Cloid {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_text_and_json() {
        let c = Cloid::from_u128(1);
        assert_eq!(c.to_string(), "0x00000000000000000000000000000001");
        assert_eq!(c.to_string().parse::<Cloid>().unwrap(), c);
        assert_eq!(
            serde_json::to_string(&c).unwrap(),
            "\"0x00000000000000000000000000000001\""
        );
        let back: Cloid = serde_json::from_str("\"0x00000000000000000000000000000001\"").unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn rejects_bad_text() {
        for bad in [
            "",
            "0x",
            "0x01",
            "00000000000000000000000000000001",
            "0xzz000000000000000000000000000001",
        ] {
            assert!(bad.parse::<Cloid>().is_err(), "{bad}");
        }
    }

    #[test]
    fn random_cloids_differ() {
        assert_ne!(Cloid::random().unwrap(), Cloid::random().unwrap());
    }
}
