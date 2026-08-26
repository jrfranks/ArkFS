//! Durable temporal index codec (`ARKIDX2`, readable `ARKIDX1`).
//!
//! The index is itself a CAS object. The store anchor `temporal_index` holds
//! its ObjectId (32 raw bytes). Restarts: read anchor → get object → decode.
//!
//! # V2 record (current)
//!
//! Per version: content id, attrs id, logical, wall, parent index, tombstone,
//! `file_id`, `file_type`. V1 lacked tombstone/file_id/type (decoded as
//! live file with `file_id = 0`). Do not write V1.
//!
//! `PathKey::new` is used on decode (keys were canonical when written).

use arkfs_core::codec::{Reader, Writer};
use arkfs_core::{ArkError, FileType, ObjectId, PathKey, Timestamp};
use std::collections::BTreeMap;

/// One cactus node for a path. `parent` is the index in `versions` (not a file_id).
#[derive(Debug, Clone)]
pub struct VersionRecord {
    pub content_id: ObjectId,
    pub attrs_id: ObjectId,
    pub at: Timestamp,
    pub parent: Option<u32>,
    pub tombstone: bool,
    pub file_id: u64,
    pub file_type: FileType,
}

/// Ordered versions for one path. Last element is the live/as-of candidate.
#[derive(Debug, Clone, Default)]
pub struct PathHistory {
    pub versions: Vec<VersionRecord>,
}

pub const INDEX_MAGIC_V1: &[u8] = b"ARKIDX1";
pub const INDEX_MAGIC_V2: &[u8] = b"ARKIDX2";

/// Whole namespace. `next_file_id` is the next unused inode (root uses 1).
#[derive(Debug, Clone, Default)]
pub struct DurableIndex {
    pub next_file_id: u64,
    pub paths: BTreeMap<PathKey, PathHistory>,
}

/// Encode as `ARKIDX2`. No checksum trailer; integrity is the object's ObjectId.
pub fn encode_index(idx: &DurableIndex) -> Vec<u8> {
    let mut w = Writer::with_magic(INDEX_MAGIC_V2);
    w.u64(idx.next_file_id);
    w.u32(idx.paths.len() as u32);
    for (path, hist) in &idx.paths {
        w.str(path.as_str());
        w.u32(hist.versions.len() as u32);
        for v in &hist.versions {
            w.arr32(v.content_id.as_bytes());
            w.arr32(v.attrs_id.as_bytes());
            w.u64(v.at.logical);
            w.u64(v.at.wall_nanos);
            w.opt_u32(v.parent);
            w.bool(v.tombstone);
            w.u64(v.file_id);
            w.u8(encode_file_type(v.file_type));
        }
    }
    w.into_inner()
}

/// Accept V2 or V1 magic. Unknown magic is Integrity.
pub fn decode_index(data: &[u8]) -> Result<DurableIndex, ArkError> {
    if data.starts_with(INDEX_MAGIC_V2) {
        decode_v2(data)
    } else if data.starts_with(INDEX_MAGIC_V1) {
        decode_v1(data)
    } else {
        Err(ArkError::integrity("bad index magic"))
    }
}

fn decode_v2(data: &[u8]) -> Result<DurableIndex, ArkError> {
    let mut r = Reader::new(data);
    r.expect_magic(INDEX_MAGIC_V2)?;
    let mut idx = DurableIndex {
        next_file_id: r.u64()?,
        paths: BTreeMap::new(),
    };
    let npaths = r.u32()? as usize;
    for _ in 0..npaths {
        let path = PathKey::new(r.str()?);
        let nver = r.u32()? as usize;
        let mut versions = Vec::with_capacity(nver);
        for _ in 0..nver {
            versions.push(VersionRecord {
                content_id: ObjectId(r.arr32()?),
                attrs_id: ObjectId(r.arr32()?),
                at: Timestamp::new(r.u64()?, r.u64()?),
                parent: r.opt_u32()?,
                tombstone: r.bool()?,
                file_id: r.u64()?,
                file_type: decode_file_type(r.u8()?)?,
            });
        }
        idx.paths.insert(path, PathHistory { versions });
    }
    r.finish()?;
    Ok(idx)
}

fn decode_v1(data: &[u8]) -> Result<DurableIndex, ArkError> {
    let mut r = Reader::new(data);
    r.expect_magic(INDEX_MAGIC_V1)?;
    let mut idx = DurableIndex {
        next_file_id: r.u64()?,
        paths: BTreeMap::new(),
    };
    let npaths = r.u32()? as usize;
    for _ in 0..npaths {
        let path = PathKey::new(r.str()?);
        let nver = r.u32()? as usize;
        let mut versions = Vec::with_capacity(nver);
        for _ in 0..nver {
            versions.push(VersionRecord {
                content_id: ObjectId(r.arr32()?),
                attrs_id: ObjectId(r.arr32()?),
                at: Timestamp::new(r.u64()?, r.u64()?),
                parent: r.opt_u32()?,
                tombstone: false,
                file_id: 0,
                file_type: FileType::File,
            });
        }
        idx.paths.insert(path, PathHistory { versions });
    }
    r.finish()?;
    Ok(idx)
}

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
