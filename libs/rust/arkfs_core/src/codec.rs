//! Lossless binary encoding for persisted ArkFS records.
//!
//! Attribute bodies are checksummed with BLAKE3 (trailer). Callers that need a
//! content id hash the whole encoded buffer with [`crate::ObjectId::from_bytes`].
//!
//! # Format (`ARKA1`)
//!
//! ```text
//! magic "ARKA1\n" | fields in encode_attr_body order | 32-byte BLAKE3(body)
//! ```
//!
//! Integers are little-endian. `bytes`/`str` are `u32 length` then payload.
//! Options are tag `0` (none) or `1` then value. Decode fails closed on bad
//! magic, truncated input, unknown tags, non-UTF-8 strings, or trailer mismatch.
//!
//! [`Writer`] / [`Reader`] are also used by `temporal_core::index` (`ARKIDX2`).
//! If you change a helper, both attr records and the index must still round-trip.
//!
//! This is **not** serde/JSON. The types derive Serialize for tests and
//! harness snapshots; on-disk truth is this codec.

use crate::attributes::{
    AceAccess, AceFlags, AceType, AclEntry, DosFlags, FileAttributes, FileType, MacOsFlags,
    NamedStream, Principal, Timespec,
};
use crate::error::ArkError;

pub const ATTR_MAGIC: &[u8] = b"ARKA1\n";

/// Growing little-endian buffer. Prefer the typed `u32`/`str` helpers over `raw`.
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// Empty buffer, no magic yet.
    pub fn new() -> Self {
        Writer { buf: Vec::new() }
    }

    /// Start a record with a magic prefix (ARKA1 / ARKIDX2).
    pub fn with_magic(magic: &[u8]) -> Self {
        let mut w = Writer::new();
        w.raw(magic);
        w
    }

    /// Take the encoded bytes.
    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    /// Append bytes with no length prefix (magic, trailers).
    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// Append one byte.
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Append 0 or 1.
    pub fn bool(&mut self, v: bool) {
        self.u8(u8::from(v));
    }

    /// Append little-endian u32.
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Append little-endian u64.
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Append little-endian i64.
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Append 32 raw bytes (ObjectId).
    pub fn arr32(&mut self, v: &[u8; 32]) {
        self.buf.extend_from_slice(v);
    }

    /// u32 length then payload.
    pub fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.buf.extend_from_slice(v);
    }

    /// UTF-8 string as length-prefixed bytes.
    pub fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }

    /// Tag 0 none / 1 then str.
    pub fn opt_str(&mut self, v: Option<&str>) {
        match v {
            None => self.u8(0),
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
        }
    }

    /// Tag 0 none / 1 then bytes.
    pub fn opt_bytes(&mut self, v: Option<&[u8]>) {
        match v {
            None => self.u8(0),
            Some(b) => {
                self.u8(1);
                self.bytes(b);
            }
        }
    }

    /// Tag 0 none / 1 then u32.
    pub fn opt_u32(&mut self, v: Option<u32>) {
        match v {
            None => self.u8(0),
            Some(x) => {
                self.u8(1);
                self.u32(x);
            }
        }
    }

    /// Tag 0 none / 1 then u64.
    pub fn opt_u64(&mut self, v: Option<u64>) {
        match v {
            None => self.u8(0),
            Some(x) => {
                self.u8(1);
                self.u64(x);
            }
        }
    }

    /// Tag 0 none / 1 then 32 bytes.
    pub fn opt_arr32(&mut self, v: Option<&[u8; 32]>) {
        match v {
            None => self.u8(0),
            Some(a) => {
                self.u8(1);
                self.arr32(a);
            }
        }
    }

    /// i64 sec then u32 nsec.
    pub fn timespec(&mut self, t: Timespec) {
        self.i64(t.sec);
        self.u32(t.nsec);
    }
}

impl Default for Writer {
    /// Same as Writer::new.
    fn default() -> Self {
        Self::new()
    }
}

/// Cursor over an encoded buffer. `finish` errors if any bytes remain.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Cursor at byte 0.
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    /// Read and require an exact magic prefix.
    pub fn expect_magic(&mut self, magic: &[u8]) -> Result<(), ArkError> {
        let got = self.take(magic.len())?;
        if got != magic {
            return Err(ArkError::integrity("bad magic"));
        }
        Ok(())
    }

    /// Unread byte count.
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// Next n bytes or Integrity (truncated/overflow).
    fn take(&mut self, n: usize) -> Result<&'a [u8], ArkError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| ArkError::integrity("overflow"))?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| ArkError::integrity("truncated"))?;
        self.pos = end;
        Ok(s)
    }

    /// Read one byte.
    pub fn u8(&mut self) -> Result<u8, ArkError> {
        Ok(self.take(1)?[0])
    }

    /// Read 0/1; other tags are Integrity.
    pub fn bool(&mut self) -> Result<bool, ArkError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            t => Err(ArkError::integrity(format!("bad bool {t}"))),
        }
    }

    /// Read little-endian u32.
    pub fn u32(&mut self) -> Result<u32, ArkError> {
        let mut b = [0u8; 4];
        b.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(b))
    }

    /// Read little-endian u64.
    pub fn u64(&mut self) -> Result<u64, ArkError> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(b))
    }

    /// Read little-endian i64.
    pub fn i64(&mut self) -> Result<i64, ArkError> {
        let mut b = [0u8; 8];
        b.copy_from_slice(self.take(8)?);
        Ok(i64::from_le_bytes(b))
    }

    /// Read 32 raw bytes.
    pub fn arr32(&mut self) -> Result<[u8; 32], ArkError> {
        let s = self.take(32)?;
        let mut a = [0u8; 32];
        a.copy_from_slice(s);
        Ok(a)
    }

    /// u32 length then that many bytes.
    pub fn bytes(&mut self) -> Result<Vec<u8>, ArkError> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    /// Length-prefixed bytes as UTF-8.
    pub fn str(&mut self) -> Result<String, ArkError> {
        String::from_utf8(self.bytes()?).map_err(|e| ArkError::integrity(e.to_string()))
    }

    /// Option tag: 0 none, 1 some, else Integrity.
    fn opt_tag(&mut self) -> Result<bool, ArkError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            t => Err(ArkError::integrity(format!("bad option tag {t}"))),
        }
    }

    /// Optional UTF-8 string.
    pub fn opt_str(&mut self) -> Result<Option<String>, ArkError> {
        if self.opt_tag()? {
            Ok(Some(self.str()?))
        } else {
            Ok(None)
        }
    }

    /// Optional length-prefixed bytes.
    pub fn opt_bytes(&mut self) -> Result<Option<Vec<u8>>, ArkError> {
        if self.opt_tag()? {
            Ok(Some(self.bytes()?))
        } else {
            Ok(None)
        }
    }

    /// Optional u32.
    pub fn opt_u32(&mut self) -> Result<Option<u32>, ArkError> {
        if self.opt_tag()? {
            Ok(Some(self.u32()?))
        } else {
            Ok(None)
        }
    }

    /// Optional u64.
    pub fn opt_u64(&mut self) -> Result<Option<u64>, ArkError> {
        if self.opt_tag()? {
            Ok(Some(self.u64()?))
        } else {
            Ok(None)
        }
    }

    /// Optional 32-byte array.
    pub fn opt_arr32(&mut self) -> Result<Option<[u8; 32]>, ArkError> {
        if self.opt_tag()? {
            Ok(Some(self.arr32()?))
        } else {
            Ok(None)
        }
    }

    /// Read sec/nsec into Timespec.
    pub fn timespec(&mut self) -> Result<Timespec, ArkError> {
        Ok(Timespec::new(self.i64()?, self.u32()?))
    }

    /// Error if any unread bytes remain.
    pub fn finish(self) -> Result<(), ArkError> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err(ArkError::integrity("trailing bytes"))
        }
    }
}

/// FileType → ARKA1 u8 tag.
fn encode_file_type(t: FileType) -> u8 {
    match t {
        FileType::File => 0,
        FileType::Directory => 1,
        FileType::Symlink => 2,
        FileType::BlockDevice => 3,
        FileType::CharDevice => 4,
        FileType::Fifo => 5,
        FileType::Socket => 6,
        FileType::Reparse => 7,
    }
}

/// ARKA1 u8 tag → FileType.
fn decode_file_type(t: u8) -> Result<FileType, ArkError> {
    match t {
        0 => Ok(FileType::File),
        1 => Ok(FileType::Directory),
        2 => Ok(FileType::Symlink),
        3 => Ok(FileType::BlockDevice),
        4 => Ok(FileType::CharDevice),
        5 => Ok(FileType::Fifo),
        6 => Ok(FileType::Socket),
        7 => Ok(FileType::Reparse),
        _ => Err(ArkError::integrity(format!("bad file type {t}"))),
    }
}

/// ACL principal tagged union.
fn encode_principal(w: &mut Writer, p: &Principal) {
    match p {
        Principal::Unix { uid, gid } => {
            w.u8(0);
            w.u32(*uid);
            w.opt_u32(*gid);
        }
        Principal::Name(s) => {
            w.u8(1);
            w.str(s);
        }
        Principal::Sid(s) => {
            w.u8(2);
            w.str(s);
        }
        Principal::Everyone => w.u8(3),
        Principal::Authenticated => w.u8(4),
        Principal::Owner => w.u8(5),
        Principal::Group => w.u8(6),
    }
}

/// ACL principal tagged union.
fn decode_principal(r: &mut Reader<'_>) -> Result<Principal, ArkError> {
    match r.u8()? {
        0 => Ok(Principal::Unix {
            uid: r.u32()?,
            gid: r.opt_u32()?,
        }),
        1 => Ok(Principal::Name(r.str()?)),
        2 => Ok(Principal::Sid(r.str()?)),
        3 => Ok(Principal::Everyone),
        4 => Ok(Principal::Authenticated),
        5 => Ok(Principal::Owner),
        6 => Ok(Principal::Group),
        t => Err(ArkError::integrity(format!("bad principal {t}"))),
    }
}

/// Allow/Deny/Audit/Alarm as u8.
fn encode_ace_type(t: AceType) -> u8 {
    match t {
        AceType::Allow => 0,
        AceType::Deny => 1,
        AceType::Audit => 2,
        AceType::Alarm => 3,
    }
}

/// u8 → AceType.
fn decode_ace_type(t: u8) -> Result<AceType, ArkError> {
    match t {
        0 => Ok(AceType::Allow),
        1 => Ok(AceType::Deny),
        2 => Ok(AceType::Audit),
        3 => Ok(AceType::Alarm),
        _ => Err(ArkError::integrity(format!("bad ace type {t}"))),
    }
}

/// AceAccess flags as packed bools.
fn encode_access(w: &mut Writer, a: &AceAccess) {
    w.bool(a.read_data);
    w.bool(a.write_data);
    w.bool(a.append_data);
    w.bool(a.read_attrs);
    w.bool(a.write_attrs);
    w.bool(a.read_named_attrs);
    w.bool(a.write_named_attrs);
    w.bool(a.execute);
    w.bool(a.delete_child);
    w.bool(a.read_acl);
    w.bool(a.write_acl);
    w.bool(a.write_owner);
    w.bool(a.synchronize);
    w.bool(a.delete);
}

/// Packed bools → AceAccess.
fn decode_access(r: &mut Reader<'_>) -> Result<AceAccess, ArkError> {
    Ok(AceAccess {
        read_data: r.bool()?,
        write_data: r.bool()?,
        append_data: r.bool()?,
        read_attrs: r.bool()?,
        write_attrs: r.bool()?,
        read_named_attrs: r.bool()?,
        write_named_attrs: r.bool()?,
        execute: r.bool()?,
        delete_child: r.bool()?,
        read_acl: r.bool()?,
        write_acl: r.bool()?,
        write_owner: r.bool()?,
        synchronize: r.bool()?,
        delete: r.bool()?,
    })
}

/// Inherit/audit AceFlags as packed bools.
fn encode_ace_flags(w: &mut Writer, f: &AceFlags) {
    w.bool(f.file_inherit);
    w.bool(f.dir_inherit);
    w.bool(f.no_propagate);
    w.bool(f.inherit_only);
    w.bool(f.inherited);
    w.bool(f.successful_access);
    w.bool(f.failed_access);
}

/// Packed bools → AceFlags.
fn decode_ace_flags(r: &mut Reader<'_>) -> Result<AceFlags, ArkError> {
    Ok(AceFlags {
        file_inherit: r.bool()?,
        dir_inherit: r.bool()?,
        no_propagate: r.bool()?,
        inherit_only: r.bool()?,
        inherited: r.bool()?,
        successful_access: r.bool()?,
        failed_access: r.bool()?,
    })
}

/// DOS/SMB flags as packed bools.
fn encode_dos(w: &mut Writer, d: &DosFlags) {
    w.bool(d.readonly);
    w.bool(d.hidden);
    w.bool(d.system);
    w.bool(d.archive);
    w.bool(d.temporary);
    w.bool(d.sparse);
    w.bool(d.reparse);
    w.bool(d.compressed);
    w.bool(d.offline);
    w.bool(d.not_content_indexed);
    w.bool(d.encrypted);
    w.bool(d.integrity_stream);
    w.bool(d.no_scrub_data);
    w.bool(d.directory);
}

/// Packed bools → DosFlags.
fn decode_dos(r: &mut Reader<'_>) -> Result<DosFlags, ArkError> {
    Ok(DosFlags {
        readonly: r.bool()?,
        hidden: r.bool()?,
        system: r.bool()?,
        archive: r.bool()?,
        temporary: r.bool()?,
        sparse: r.bool()?,
        reparse: r.bool()?,
        compressed: r.bool()?,
        offline: r.bool()?,
        not_content_indexed: r.bool()?,
        encrypted: r.bool()?,
        integrity_stream: r.bool()?,
        no_scrub_data: r.bool()?,
        directory: r.bool()?,
    })
}

/// macOS UF_*/SF_* flags as packed bools.
fn encode_macos(w: &mut Writer, m: &MacOsFlags) {
    w.bool(m.uf_nodump);
    w.bool(m.uf_immutable);
    w.bool(m.uf_append);
    w.bool(m.uf_opaque);
    w.bool(m.uf_hidden);
    w.bool(m.uf_compressed);
    w.bool(m.uf_tracked);
    w.bool(m.uf_datavault);
    w.bool(m.sf_archived);
    w.bool(m.sf_immutable);
    w.bool(m.sf_append);
    w.bool(m.sf_restricted);
    w.bool(m.sf_nounlink);
}

/// Packed bools → MacOsFlags.
fn decode_macos(r: &mut Reader<'_>) -> Result<MacOsFlags, ArkError> {
    Ok(MacOsFlags {
        uf_nodump: r.bool()?,
        uf_immutable: r.bool()?,
        uf_append: r.bool()?,
        uf_opaque: r.bool()?,
        uf_hidden: r.bool()?,
        uf_compressed: r.bool()?,
        uf_tracked: r.bool()?,
        uf_datavault: r.bool()?,
        sf_archived: r.bool()?,
        sf_immutable: r.bool()?,
        sf_append: r.bool()?,
        sf_restricted: r.bool()?,
        sf_nounlink: r.bool()?,
    })
}

/// Encode attribute fields (no checksum trailer).
pub fn encode_attr_body(attrs: &FileAttributes) -> Vec<u8> {
    let mut w = Writer::with_magic(ATTR_MAGIC);
    w.u64(attrs.file_id);
    w.u64(attrs.generation);
    w.u8(encode_file_type(attrs.file_type));
    w.u32(attrs.mode);
    w.u32(attrs.nlink);
    w.u32(attrs.uid);
    w.u32(attrs.gid);
    w.opt_str(attrs.owner_name.as_deref());
    w.opt_str(attrs.group_name.as_deref());
    w.u64(attrs.logical_size);
    w.u64(attrs.allocation_size);
    w.timespec(attrs.atime);
    w.timespec(attrs.mtime);
    w.timespec(attrs.ctime);
    w.timespec(attrs.btime);
    w.u64(attrs.change_attr);
    encode_dos(&mut w, &attrs.dos);
    encode_macos(&mut w, &attrs.macos);
    w.u32(attrs.acl.len() as u32);
    for ace in &attrs.acl {
        encode_principal(&mut w, &ace.principal);
        w.u8(encode_ace_type(ace.ace_type));
        encode_access(&mut w, &ace.access);
        encode_ace_flags(&mut w, &ace.flags);
    }
    w.opt_bytes(attrs.security_descriptor.as_deref());
    w.u32(attrs.xattrs.len() as u32);
    for (k, v) in &attrs.xattrs {
        w.str(k);
        w.bytes(v);
    }
    w.u32(attrs.streams.len() as u32);
    for s in &attrs.streams {
        w.str(&s.name);
        w.u64(s.size);
        w.opt_arr32(s.content_id.as_ref());
    }
    w.opt_str(attrs.symlink_target.as_deref());
    w.opt_u32(attrs.reparse_tag);
    w.opt_bytes(attrs.reparse_buffer.as_deref());
    w.opt_str(attrs.content_type.as_deref());
    w.opt_str(attrs.etag.as_deref());
    w.u32(attrs.dead_props.len() as u32);
    for (k, v) in &attrs.dead_props {
        w.str(k);
        w.str(v);
    }
    w.opt_u64(attrs.rdev);
    w.into_inner()
}

/// Decode fields after ARKA1 magic; caller checks the BLAKE3 trailer.
fn decode_attr_body(data: &[u8]) -> Result<FileAttributes, ArkError> {
    let mut r = Reader::new(data);
    r.expect_magic(ATTR_MAGIC)?;
    let mut attrs = FileAttributes {
        file_id: r.u64()?,
        generation: r.u64()?,
        file_type: decode_file_type(r.u8()?)?,
        mode: r.u32()?,
        nlink: r.u32()?,
        uid: r.u32()?,
        gid: r.u32()?,
        owner_name: r.opt_str()?,
        group_name: r.opt_str()?,
        logical_size: r.u64()?,
        allocation_size: r.u64()?,
        atime: r.timespec()?,
        mtime: r.timespec()?,
        ctime: r.timespec()?,
        btime: r.timespec()?,
        change_attr: r.u64()?,
        dos: decode_dos(&mut r)?,
        macos: decode_macos(&mut r)?,
        acl: Vec::new(),
        security_descriptor: None,
        xattrs: Default::default(),
        streams: Vec::new(),
        symlink_target: None,
        reparse_tag: None,
        reparse_buffer: None,
        content_type: None,
        etag: None,
        dead_props: Default::default(),
        rdev: None,
        attr_checksum: None,
    };
    let nacl = r.u32()? as usize;
    attrs.acl.reserve(nacl);
    for _ in 0..nacl {
        attrs.acl.push(AclEntry {
            principal: decode_principal(&mut r)?,
            ace_type: decode_ace_type(r.u8()?)?,
            access: decode_access(&mut r)?,
            flags: decode_ace_flags(&mut r)?,
        });
    }
    attrs.security_descriptor = r.opt_bytes()?;
    let nx = r.u32()? as usize;
    for _ in 0..nx {
        let k = r.str()?;
        let v = r.bytes()?;
        attrs.xattrs.insert(k, v);
    }
    let ns = r.u32()? as usize;
    attrs.streams.reserve(ns);
    for _ in 0..ns {
        attrs.streams.push(NamedStream {
            name: r.str()?,
            size: r.u64()?,
            content_id: r.opt_arr32()?,
        });
    }
    attrs.symlink_target = r.opt_str()?;
    attrs.reparse_tag = r.opt_u32()?;
    attrs.reparse_buffer = r.opt_bytes()?;
    attrs.content_type = r.opt_str()?;
    attrs.etag = r.opt_str()?;
    let np = r.u32()? as usize;
    for _ in 0..np {
        let k = r.str()?;
        let v = r.str()?;
        attrs.dead_props.insert(k, v);
    }
    attrs.rdev = r.opt_u64()?;
    r.finish()?;
    Ok(attrs)
}

/// Encode attributes with a BLAKE3 checksum trailer.
pub fn encode_attrs(attrs: &FileAttributes) -> Vec<u8> {
    let mut body = encode_attr_body(attrs);
    let hash = blake3::hash(&body);
    body.extend_from_slice(hash.as_bytes());
    body
}

/// Decode attributes and verify the checksum trailer.
pub fn decode_attrs(data: &[u8]) -> Result<FileAttributes, ArkError> {
    if data.len() < 32 {
        return Err(ArkError::integrity("attr record too short"));
    }
    let (body, sum) = data.split_at(data.len() - 32);
    let expect = blake3::hash(body);
    if expect.as_bytes() != sum {
        return Err(ArkError::integrity("attr checksum mismatch"));
    }
    let mut attrs = decode_attr_body(body)?;
    let mut checksum = [0u8; 32];
    checksum.copy_from_slice(sum);
    attrs.attr_checksum = Some(checksum);
    Ok(attrs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::{AceType, FileType, Principal};
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// Fixture FileAttributes with every optional field set (lossless tests).
    fn rich() -> FileAttributes {
        let mut a = FileAttributes::new_dir(9, 0o755);
        a.generation = 3;
        a.owner_name = Some("alice".into());
        a.atime = Timespec::new(1, 2);
        a.mtime = Timespec::new(3, 4);
        a.ctime = Timespec::new(5, 6);
        a.btime = Timespec::new(7, 8);
        a.dos.archive = true;
        a.dos.readonly = true;
        a.macos.uf_hidden = true;
        a.macos.sf_append = true;
        a.acl.push(AclEntry {
            principal: Principal::Everyone,
            ace_type: AceType::Allow,
            access: AceAccess {
                read_data: true,
                ..Default::default()
            },
            flags: AceFlags {
                file_inherit: true,
                ..Default::default()
            },
        });
        a.xattrs.insert("user.a".into(), b"v".to_vec());
        a.dead_props.insert("{DAV:}x".into(), "y".into());
        a.content_type = Some("text/plain".into());
        a.etag = Some("\"1\"".into());
        a.rdev = None;
        a.symlink_target = None;
        a
    }

    /// encode/decode a rich record and a directory without dropping fields.
    #[test]
    fn directory_roundtrip_is_lossless() {
        let _g = arkfs_test_review::guard();
        let a = rich();
        let bytes = encode_attrs(&a);
        let b = decode_attrs(&bytes).unwrap();
        assert_eq!(b.file_type, FileType::Directory);
        assert_eq!(b.nlink, 2);
        assert!(b.dos.directory);
        assert!(b.dos.archive);
        assert!(b.dos.readonly);
        assert!(b.macos.sf_append);
        assert_eq!(b.atime.nsec, 2);
        assert_eq!(b.generation, 3);
        assert_eq!(b.acl.len(), 1);
        assert_eq!(b.xattrs.get("user.a").map(Vec::as_slice), Some(&b"v"[..]));
        assert!(b.streams.is_empty());
        assert_eq!(b.attr_checksum.map(|c| c.len()), Some(32));
        b.attr_checksum.unwrap();
        let mut a2 = a.clone();
        a2.attr_checksum = b.attr_checksum;
        assert_eq!(a2, b);
        let trailer = {
            let encoded = encode_attrs(&a);
            let mut sum = [0u8; 32];
            sum.copy_from_slice(&encoded[encoded.len() - 32..]);
            sum
        };
        assert_eq!(a.compute_checksum(), trailer);
    }

    /// Flipping the BLAKE3 trailer is Integrity.
    #[test]
    fn corrupt_trailer_fails() {
        let _g = arkfs_test_review::guard();
        let mut bytes = encode_attrs(&FileAttributes::new_file(1, 0o644));
        let n = bytes.len();
        bytes[n - 1] ^= 1;
        assert!(decode_attrs(&bytes).is_err());
    }
}
