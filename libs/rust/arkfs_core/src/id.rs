//! Content ids, canonical paths, and owner identity.
//!
//! [`ObjectId`] is the CAS key: `filename = hex(blake3(payload)) + ".obj"`.
//! [`PathKey`] is the namespace key in the temporal index. Do not store raw
//! user strings as paths — always [`PathKey::parse`] (or `join` from a parent).

use crate::error::ArkError;
use serde::{Deserialize, Serialize};
use std::fmt;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Content-addressed object identifier (BLAKE3-256 of the payload bytes).
///
/// Two identical payloads share an id (dedup). `from_labeled` is for domain
/// separation when the same bytes must not collide across roles (attrs vs
/// content). The public field is the raw 32 bytes; prefer `from_bytes` /
/// `from_hex` over constructing `ObjectId([...])` except when reading an
/// already-validated anchor.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectId(pub [u8; 32]);

impl ObjectId {
    /// Hash `data` with unkeyed BLAKE3. This is the store's object name.
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

    /// Lowercase 64-char hex. Object files are `{to_hex()}.obj`.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0x0f) as usize] as char);
        }
        s
    }

    /// Inverse of [`Self::to_hex`]. Rejects wrong length and non-hex digits.
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

/// Canonical absolute path used as the temporal index key.
///
/// Invariants after [`parse`](Self::parse) / [`join`](Self::join):
/// starts with `/`, no NUL, no `.` or `..` components, no trailing slash
/// except root, no empty components (`//` collapsed).
///
/// [`new`](Self::new) is **unchecked** — only for index decode of already
/// stored keys. FUSE names go through `join` so a slash in a component is
/// `InvalidArgument`, not a surprising nested path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PathKey(pub String);

impl PathKey {
    pub fn root() -> Self {
        PathKey("/".into())
    }

    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }

    /// Unchecked constructor for already-canonical keys (index decode).
    pub fn new(path: impl Into<String>) -> Self {
        PathKey(path.into())
    }

    /// Absolute path without `.` / `..` / NUL. Trailing slashes stripped except `/`.
    pub fn parse(path: impl AsRef<str>) -> Result<Self, ArkError> {
        let s = path.as_ref();
        if s.is_empty() || !s.starts_with('/') {
            return Err(ArkError::invalid_argument("path must be absolute"));
        }
        for b in s.as_bytes() {
            if *b == 0 {
                return Err(ArkError::invalid_argument("path contains NUL"));
            }
        }
        let mut parts: Vec<&str> = Vec::new();
        for part in s.split('/') {
            if part.is_empty() {
                continue;
            }
            if part == "." || part == ".." {
                return Err(ArkError::invalid_argument("path must not contain . or .."));
            }
            parts.push(part);
        }
        if parts.is_empty() {
            return Ok(Self::root());
        }
        let mut out = String::from("/");
        out.push_str(&parts.join("/"));
        Ok(PathKey(out))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Final component (`"/"` for root, `"c"` for `"/a/b/c"`).
    pub fn name(&self) -> &str {
        if self.is_root() {
            "/"
        } else {
            self.0.rsplit('/').next().unwrap_or("")
        }
    }

    /// Parent directory, or `None` at root (root has no parent in this model).
    pub fn parent(&self) -> Option<PathKey> {
        if self.is_root() {
            return None;
        }
        match self.0.rfind('/') {
            Some(0) | None => Some(Self::root()),
            Some(i) => Some(PathKey(self.0[..i].to_string())),
        }
    }

    /// Append one component. `name` must not contain `/`, NUL, `.`, or `..`.
    pub fn join(&self, name: &str) -> Result<PathKey, ArkError> {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\0')
            || name == "."
            || name == ".."
        {
            return Err(ArkError::invalid_argument("invalid path component"));
        }
        if self.is_root() {
            PathKey::parse(format!("/{name}"))
        } else {
            PathKey::parse(format!("{}/{name}", self.0))
        }
    }

    /// Immediate child name if `child` is a direct child of `self`.
    pub fn immediate_child<'a>(&self, child: &'a PathKey) -> Option<&'a str> {
        if self.is_root() {
            let rest = child.as_str().strip_prefix('/')?;
            if rest.is_empty() || rest.contains('/') {
                return None;
            }
            return Some(rest);
        }
        let prefix = format!("{}/", self.0);
        let rest = child.as_str().strip_prefix(&prefix)?;
        if rest.is_empty() || rest.contains('/') {
            return None;
        }
        Some(rest)
    }
}

/// Cluster / data owner identity (opaque string). Unused by single-node FUSE.
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
    fn path_parse_and_join() {
        let p = PathKey::parse("/a/b/c").unwrap();
        assert_eq!(p.as_str(), "/a/b/c");
        assert_eq!(p.name(), "c");
        assert_eq!(p.parent().unwrap().as_str(), "/a/b");
        assert_eq!(p.parent().unwrap().parent().unwrap().as_str(), "/a");
        assert!(p
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .is_root());
        assert_eq!(
            PathKey::root()
                .join("a")
                .unwrap()
                .join("b")
                .unwrap()
                .as_str(),
            "/a/b"
        );
        assert_eq!(
            PathKey::parse("/a")
                .unwrap()
                .immediate_child(&PathKey::parse("/a/b").unwrap()),
            Some("b")
        );
        assert!(PathKey::parse("/a")
            .unwrap()
            .immediate_child(&PathKey::parse("/a/b/c").unwrap())
            .is_none());
        assert!(PathKey::parse("rel").is_err());
        assert!(PathKey::parse("/a/../b").is_err());
        assert_eq!(PathKey::parse("/a//b/").unwrap().as_str(), "/a/b");
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
