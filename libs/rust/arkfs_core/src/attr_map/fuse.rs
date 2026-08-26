//! FUSE getattr/setattr projection. Does not talk to the kernel; `fuse_facade`
//! maps [`FuseStat`] onto `fuser::FileAttr`.

use crate::attributes::{FileAttributes, FileType, PosixPatch, SizePolicy, Timespec};

/// FUSE-facing stat projection (POSIX-oriented). `mode` includes `S_IF*` bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseStat {
    pub ino: u64,
    pub generation: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub file_type: FileType,
}

/// Partial FUSE setattr; unset fields leave canonical values alone.
pub type FuseSetAttr = PosixPatch;

/// Project canonical attrs to FUSE stat. `blocks` is allocation_size / 512 (ceil).
pub fn to_fuse(attrs: &FileAttributes) -> FuseStat {
    let blocks = attrs.allocation_size.div_ceil(512);
    FuseStat {
        ino: attrs.file_id,
        generation: attrs.generation,
        mode: attrs.mode | type_to_s_ifmt(attrs.file_type),
        nlink: attrs.nlink,
        uid: attrs.uid,
        gid: attrs.gid,
        rdev: attrs.rdev.unwrap_or(0),
        size: attrs.logical_size,
        blocks,
        atime: attrs.atime,
        mtime: attrs.mtime,
        ctime: attrs.ctime,
        file_type: attrs.file_type,
    }
}

/// Apply a FUSE setattr patch. Unset fields (including gid/uid) stay put.
pub fn merge_from_fuse(attrs: &mut FileAttributes, patch: &FuseSetAttr, now: Timespec) {
    attrs.apply_posix(patch, now, SizePolicy::GrowAllocation);
}

fn type_to_s_ifmt(t: FileType) -> u32 {
    match t {
        FileType::File => 0o100000,
        FileType::Directory => 0o040000,
        FileType::Symlink => 0o120000,
        FileType::BlockDevice => 0o060000,
        FileType::CharDevice => 0o020000,
        FileType::Fifo => 0o010000,
        FileType::Socket => 0o140000,
        FileType::Reparse => 0o100000,
    }
}
