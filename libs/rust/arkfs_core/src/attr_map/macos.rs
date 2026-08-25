use crate::attributes::{FileAttributes, MacOsFlags, NamedStream, Timespec};
use std::collections::BTreeMap;

/// macOS-oriented stat / getattrlist style view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacOsStat {
    pub file_id: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub btime: Timespec,
    pub flags: MacOsFlags,
    pub xattrs: BTreeMap<String, Vec<u8>>,
    pub has_resource_fork: bool,
}

#[derive(Debug, Clone, Default)]
pub struct MacOsSetAttr {
    pub mode: Option<u32>,
    pub flags: Option<MacOsFlags>,
    pub btime: Option<Timespec>,
    pub xattrs_set: BTreeMap<String, Vec<u8>>,
    pub xattrs_remove: Vec<String>,
}

pub fn to_macos(attrs: &FileAttributes) -> MacOsStat {
    let has_resource_fork = attrs.streams.iter().any(|s| {
        s.name == NamedStream::RESOURCE_FORK || s.name == NamedStream::RESOURCE_FORK_LEGACY
    });
    MacOsStat {
        file_id: attrs.file_id,
        mode: attrs.mode,
        nlink: attrs.nlink,
        uid: attrs.uid,
        gid: attrs.gid,
        size: attrs.logical_size,
        atime: attrs.atime,
        mtime: attrs.mtime,
        ctime: attrs.ctime,
        btime: attrs.btime,
        flags: attrs.macos,
        xattrs: attrs.xattrs.clone(),
        has_resource_fork,
    }
}

pub fn merge_from_macos(attrs: &mut FileAttributes, patch: &MacOsSetAttr, now: Timespec) {
    if let Some(mode) = patch.mode {
        attrs.mode = mode & 0o7777;
    }
    if let Some(flags) = patch.flags {
        attrs.macos = flags;
        // Mirror uf_hidden into DOS hidden when macOS sets it (does not clear DOS if false).
        if flags.uf_hidden {
            attrs.dos.hidden = true;
        }
    }
    if let Some(btime) = patch.btime {
        attrs.btime = btime;
    }
    for (k, v) in &patch.xattrs_set {
        attrs.xattrs.insert(k.clone(), v.clone());
    }
    for k in &patch.xattrs_remove {
        attrs.xattrs.remove(k);
    }
    attrs.touch_change(now);
}
