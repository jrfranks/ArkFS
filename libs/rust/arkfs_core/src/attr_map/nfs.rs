//! NFSv4 fattr / setattr projection. Not wired to a server in Phase 0; kept so
//! FUSE setattr cannot invent a second POSIX merge path later.

use crate::attributes::{FileAttributes, FileType, PosixPatch, SizePolicy, Timespec};

/// NFSv4-oriented attribute view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nfs4Fattr {
    pub fileid: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub space_used: u64,
    pub owner_name: Option<String>,
    pub group_name: Option<String>,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub time_create: Timespec,
    pub change: u64,
    pub file_type: FileType,
    pub rdev: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct Nfs4SetAttr {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub owner_name: Option<String>,
    pub group_name: Option<String>,
    pub atime: Option<Timespec>,
    pub mtime: Option<Timespec>,
    pub ctime: Option<Timespec>,
}

impl Nfs4SetAttr {
    fn posix(&self) -> PosixPatch {
        PosixPatch {
            mode: self.mode,
            uid: self.uid,
            gid: self.gid,
            size: self.size,
            atime: self.atime,
            mtime: self.mtime,
            ctime: self.ctime,
        }
    }
}

/// Project canonical attrs to NFSv4 fattr (Phase 0: used by tests, not a server).
pub fn to_nfs4(attrs: &FileAttributes) -> Nfs4Fattr {
    Nfs4Fattr {
        fileid: attrs.file_id,
        mode: attrs.mode,
        nlink: attrs.nlink,
        uid: attrs.uid,
        gid: attrs.gid,
        size: attrs.logical_size,
        space_used: attrs.allocation_size,
        owner_name: attrs.owner_name.clone(),
        group_name: attrs.group_name.clone(),
        atime: attrs.atime,
        mtime: attrs.mtime,
        ctime: attrs.ctime,
        time_create: attrs.btime,
        change: attrs.change_attr,
        file_type: attrs.file_type,
        rdev: attrs.rdev,
    }
}

/// Merge NFSv4 setattr. Owner/group names are extra POSIX fields; DOS flags stay.
pub fn merge_from_nfs4(attrs: &mut FileAttributes, patch: &Nfs4SetAttr, now: Timespec) {
    attrs.apply_posix(&patch.posix(), now, SizePolicy::Logical);
    if let Some(ref owner) = patch.owner_name {
        attrs.owner_name = Some(owner.clone());
    }
    if let Some(ref group) = patch.group_name {
        attrs.group_name = Some(group.clone());
    }
}
