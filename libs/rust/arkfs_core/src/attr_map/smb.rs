use crate::attributes::{DosFlags, FileAttributes, FileType, SizePolicy, Timespec};

/// SMB 3 FILE_NETWORK_OPEN_INFORMATION-style view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Smb3FileInfo {
    pub file_id: u64,
    pub creation_time: Timespec,
    pub last_access: Timespec,
    pub last_write: Timespec,
    pub change_time: Timespec,
    pub end_of_file: u64,
    pub allocation_size: u64,
    pub dos: DosFlags,
}

#[derive(Debug, Clone, Default)]
pub struct Smb3SetInfo {
    pub creation_time: Option<Timespec>,
    pub last_access: Option<Timespec>,
    pub last_write: Option<Timespec>,
    pub change_time: Option<Timespec>,
    pub end_of_file: Option<u64>,
    pub allocation_size: Option<u64>,
    pub dos: Option<DosFlags>,
}

pub fn to_smb3(attrs: &FileAttributes) -> Smb3FileInfo {
    let mut dos = attrs.dos;
    dos.directory = matches!(attrs.file_type, FileType::Directory);
    Smb3FileInfo {
        file_id: attrs.file_id,
        creation_time: attrs.btime,
        last_access: attrs.atime,
        last_write: attrs.mtime,
        change_time: attrs.ctime,
        end_of_file: attrs.logical_size,
        allocation_size: attrs.allocation_size,
        dos,
    }
}

pub fn merge_from_smb3(attrs: &mut FileAttributes, patch: &Smb3SetInfo, now: Timespec) {
    if let Some(t) = patch.creation_time {
        attrs.btime = t;
    }
    if let Some(t) = patch.last_access {
        attrs.atime = t;
    }
    if let Some(t) = patch.last_write {
        attrs.mtime = t;
    }
    if let Some(t) = patch.change_time {
        attrs.ctime = t;
    }
    if let Some(eof) = patch.end_of_file {
        attrs.set_logical_size(eof, SizePolicy::Logical);
    }
    if let Some(alloc) = patch.allocation_size {
        attrs.allocation_size = alloc;
    }
    if let Some(mut dos) = patch.dos {
        dos.directory = matches!(attrs.file_type, FileType::Directory);
        attrs.dos = dos;
    }
    if patch.change_time.is_some() {
        attrs.bump_change();
    } else {
        attrs.touch_change(now);
    }
}
