//! Canonical file attribute superset for FUSE, NFS, SMB 3, WebDAV, and macOS.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Nanosecond-resolution wall time used in protocol-facing fields.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash, Serialize, Deserialize,
)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: u32,
}

impl Timespec {
    pub fn new(sec: i64, nsec: u32) -> Self {
        Timespec { sec, nsec }
    }

    pub fn from_nanos(nanos: u64) -> Self {
        Timespec {
            sec: (nanos / 1_000_000_000) as i64,
            nsec: (nanos % 1_000_000_000) as u32,
        }
    }

    pub fn as_nanos(self) -> u64 {
        (self.sec.max(0) as u64)
            .saturating_mul(1_000_000_000)
            .saturating_add(self.nsec as u64)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileType {
    #[default]
    File,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    Fifo,
    Socket,
    Reparse,
}

/// DOS / SMB file attribute flags (superset bitfield as structured flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DosFlags {
    pub readonly: bool,
    pub hidden: bool,
    pub system: bool,
    pub archive: bool,
    pub temporary: bool,
    pub sparse: bool,
    pub reparse: bool,
    pub compressed: bool,
    pub offline: bool,
    pub not_content_indexed: bool,
    pub encrypted: bool,
    pub integrity_stream: bool,
    pub no_scrub_data: bool,
    pub directory: bool,
}

/// macOS UF_*/SF_* style flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MacOsFlags {
    pub uf_nodump: bool,
    pub uf_immutable: bool,
    pub uf_append: bool,
    pub uf_opaque: bool,
    pub uf_hidden: bool,
    pub uf_compressed: bool,
    pub uf_tracked: bool,
    pub uf_datavault: bool,
    pub sf_archived: bool,
    pub sf_immutable: bool,
    pub sf_append: bool,
    pub sf_restricted: bool,
    pub sf_nounlink: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Principal {
    Unix { uid: u32, gid: Option<u32> },
    Name(String),
    Sid(String),
    Everyone,
    Authenticated,
    Owner,
    Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AceType {
    Allow,
    Deny,
    Audit,
    Alarm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AceAccess {
    pub read_data: bool,
    pub write_data: bool,
    pub append_data: bool,
    pub read_attrs: bool,
    pub write_attrs: bool,
    pub read_named_attrs: bool,
    pub write_named_attrs: bool,
    pub execute: bool,
    pub delete_child: bool,
    pub read_acl: bool,
    pub write_acl: bool,
    pub write_owner: bool,
    pub synchronize: bool,
    pub delete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AceFlags {
    pub file_inherit: bool,
    pub dir_inherit: bool,
    pub no_propagate: bool,
    pub inherit_only: bool,
    pub inherited: bool,
    pub successful_access: bool,
    pub failed_access: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclEntry {
    pub principal: Principal,
    pub ace_type: AceType,
    pub access: AceAccess,
    pub flags: AceFlags,
}

/// Named data stream / fork (SMB ADS, macOS resource fork, default `::$DATA`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedStream {
    /// Empty or `::$DATA` for primary content; `:Zone.Identifier` etc. for ADS;
    /// `com.apple.ResourceFork` for resource fork.
    pub name: String,
    pub size: u64,
    pub content_id: Option<[u8; 32]>,
}

impl NamedStream {
    pub const PRIMARY: &'static str = "::$DATA";
    pub const RESOURCE_FORK: &'static str = "com.apple.ResourceFork";
    pub const RESOURCE_FORK_LEGACY: &'static str = "..namedfork/rsrc";
}

/// Optional POSIX fields shared by FUSE / NFS setattr-style patches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PosixPatch {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub atime: Option<Timespec>,
    pub mtime: Option<Timespec>,
    pub ctime: Option<Timespec>,
}

/// How [`FileAttributes::set_logical_size`] treats `allocation_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizePolicy {
    /// Update `logical_size` and the primary stream size only.
    Logical,
    /// Also raise `allocation_size` when it would fall below the new logical size.
    GrowAllocation,
}

/// Canonical attribute record — single source of truth for all protocol facades.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAttributes {
    // Identity
    pub file_id: u64,
    pub generation: u64,

    // Type
    pub file_type: FileType,

    // POSIX
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,

    // Ownership strings (NFSv4 / SMB / macOS)
    pub owner_name: Option<String>,
    pub group_name: Option<String>,

    // Size
    pub logical_size: u64,
    pub allocation_size: u64,

    // Times
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub btime: Timespec,
    /// Monotonic change attribute (NFSv4 / cache coherence).
    pub change_attr: u64,

    // Protocol-specific flags
    pub dos: DosFlags,
    pub macos: MacOsFlags,

    // ACL + optional raw security descriptor (SMB)
    pub acl: Vec<AclEntry>,
    pub security_descriptor: Option<Vec<u8>>,

    // Extended attributes
    pub xattrs: BTreeMap<String, Vec<u8>>,

    // Streams / forks (primary data may also be tracked here)
    pub streams: Vec<NamedStream>,

    // Symlink / reparse
    pub symlink_target: Option<String>,
    pub reparse_tag: Option<u32>,
    pub reparse_buffer: Option<Vec<u8>>,

    // WebDAV live + dead properties
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub dead_props: BTreeMap<String, String>,

    // Device nodes
    pub rdev: Option<u64>,

    // Integrity over attribute fields (excluding this checksum itself when computed)
    pub attr_checksum: Option<[u8; 32]>,
}

impl Default for FileAttributes {
    fn default() -> Self {
        FileAttributes {
            file_id: 0,
            generation: 0,
            file_type: FileType::File,
            mode: 0o644,
            nlink: 1,
            uid: 0,
            gid: 0,
            owner_name: None,
            group_name: None,
            logical_size: 0,
            allocation_size: 0,
            atime: Timespec::default(),
            mtime: Timespec::default(),
            ctime: Timespec::default(),
            btime: Timespec::default(),
            change_attr: 0,
            dos: DosFlags::default(),
            macos: MacOsFlags::default(),
            acl: Vec::new(),
            security_descriptor: None,
            xattrs: BTreeMap::new(),
            streams: vec![NamedStream {
                name: NamedStream::PRIMARY.into(),
                size: 0,
                content_id: None,
            }],
            symlink_target: None,
            reparse_tag: None,
            reparse_buffer: None,
            content_type: None,
            etag: None,
            dead_props: BTreeMap::new(),
            rdev: None,
            attr_checksum: None,
        }
    }
}

impl FileAttributes {
    pub fn new_file(file_id: u64, mode: u32) -> Self {
        let mut a = FileAttributes {
            file_id,
            mode,
            file_type: FileType::File,
            ..Default::default()
        };
        a.dos.archive = true;
        a
    }

    pub fn new_dir(file_id: u64, mode: u32) -> Self {
        FileAttributes {
            file_id,
            mode,
            file_type: FileType::Directory,
            nlink: 2,
            dos: DosFlags {
                directory: true,
                ..Default::default()
            },
            streams: Vec::new(),
            ..Default::default()
        }
    }

    /// Bump the monotonic change attribute (NFSv4 / cache coherence).
    pub fn bump_change(&mut self) {
        self.change_attr = self.change_attr.saturating_add(1);
    }

    /// Bump change_attr and set ctime to `now` (POSIX-style metadata change).
    pub fn touch_change(&mut self, now: Timespec) {
        self.bump_change();
        self.ctime = now;
    }

    pub fn primary_stream_mut(&mut self) -> Option<&mut NamedStream> {
        self.streams
            .iter_mut()
            .find(|s| s.name == NamedStream::PRIMARY)
    }

    pub fn set_logical_size(&mut self, size: u64, policy: SizePolicy) {
        self.logical_size = size;
        if let Some(s) = self.primary_stream_mut() {
            s.size = size;
        }
        if matches!(policy, SizePolicy::GrowAllocation) && self.allocation_size < size {
            self.allocation_size = size;
        }
    }

    pub fn apply_posix(&mut self, patch: &PosixPatch, now: Timespec, size: SizePolicy) {
        if let Some(mode) = patch.mode {
            self.mode = mode & 0o7777;
        }
        if let Some(uid) = patch.uid {
            self.uid = uid;
        }
        if let Some(gid) = patch.gid {
            self.gid = gid;
        }
        if let Some(sz) = patch.size {
            self.set_logical_size(sz, size);
        }
        if let Some(atime) = patch.atime {
            self.atime = atime;
        }
        if let Some(mtime) = patch.mtime {
            self.mtime = mtime;
        }
        if let Some(ctime) = patch.ctime {
            self.ctime = ctime;
            self.bump_change();
        } else {
            self.touch_change(now);
        }
    }

    pub fn compute_checksum(&self) -> [u8; 32] {
        *blake3::hash(&crate::codec::encode_attr_body(self)).as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_primary_stream() {
        let a = FileAttributes::default();
        assert_eq!(a.streams.len(), 1);
        assert_eq!(a.streams[0].name, NamedStream::PRIMARY);
    }

    #[test]
    fn apply_posix_grows_allocation() {
        let mut a = FileAttributes::new_file(1, 0o644);
        a.apply_posix(
            &PosixPatch {
                size: Some(100),
                mode: Some(0o600),
                ..Default::default()
            },
            Timespec::new(1, 0),
            SizePolicy::GrowAllocation,
        );
        assert_eq!(a.mode, 0o600);
        assert_eq!(a.logical_size, 100);
        assert_eq!(a.allocation_size, 100);
        assert_eq!(a.streams[0].size, 100);
    }

    #[test]
    fn touch_change_increments() {
        let mut a = FileAttributes::new_file(1, 0o644);
        a.touch_change(Timespec::new(100, 0));
        assert_eq!(a.change_attr, 1);
        assert_eq!(a.ctime.sec, 100);
    }
}
