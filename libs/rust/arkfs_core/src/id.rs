use crate::error::ArkError;
use serde::{Deserialize, Serialize};
use std::fmt;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Content-addressed object identifier (BLAKE3-256).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectId(pub [u8; 32]);

impl ObjectId {
    pub fn from_bytes(data: &[u8]) -> Self {
        ObjectId(*blake3::hash(data).as_bytes())
    }

    /// Domain-separated id: BLAKE3 keyed hash with a key derived from `label`.
    pub fn from_labeled(label: &[u8], data: &[u8]) -> Self {
        let mut key_hasher = blake3::Hasher::new_derive_key("arkfs ObjectId from_labeled v1");
        key_hasher.update(label);
        let key = *key_hasher.finalize().as_bytes();
        ObjectId(*blake3::keyed_hash(&key, data).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0x0f) as usize] as char);
        }
        s
    }

    pub fn from_hex(s: &str) -> Result<Self, ArkError> {
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return Err(ArkError::invalid_argument("object id hex must be 64 chars"));
        }
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = (hex_nibble(bytes[i * 2])? << 4) | hex_nibble(bytes[i * 2 + 1])?;
        }
        Ok(ObjectId(out))
    }
}

fn hex_nibble(c: u8) -> Result<u8, ArkError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(ArkError::invalid_argument("invalid hex digit")),
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", &self.to_hex()[..16])
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// Stable path key within the temporal namespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PathKey(pub String);

impl PathKey {
    pub fn new(path: impl Into<String>) -> Self {
        PathKey(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Cluster / data owner identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OwnerId(pub String);

impl OwnerId {
    pub fn new(id: impl Into<String>) -> Self {
        OwnerId(id.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_id_is_deterministic() {
        let a = ObjectId::from_bytes(b"hello");
        let b = ObjectId::from_bytes(b"hello");
        let c = ObjectId::from_bytes(b"world");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.to_hex().len(), 64);
        assert_eq!(ObjectId::from_hex(&a.to_hex()).unwrap(), a);
    }

    #[test]
    fn labeled_differs_from_raw_and_uses_key_derivation() {
        let raw = ObjectId::from_bytes(b"data");
        let labeled = ObjectId::from_labeled(b"attr", b"data");
        assert_ne!(raw, labeled);
        // Length-prefix confusion: label "a\0data" vs label "a" + data "data" must differ.
        let a = ObjectId::from_labeled(b"ab", b"cd");
        let b = ObjectId::from_labeled(b"a", b"bcd");
        assert_ne!(a, b);
    }
}
