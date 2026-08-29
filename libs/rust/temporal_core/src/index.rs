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

use crate::View;
use arkfs_core::codec::{Reader, Writer};
use arkfs_core::{ArkError, FileType, ObjectId, PathKey, Timestamp};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// One cactus node for a path. `parent` is the index in `versions` (not a file_id).
///
/// Maintainer: `file_id` is the inode identity. `parent` links versions for
/// this path only (cactus per-path). Tombstone versions hide live names but
/// keep prior bytes. See "Cactus stack", "Tombstone", "Inode / file_id".
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
///
/// Maintainer: versions are append-only per path. Last is candidate for Live.
/// AsOf walks backwards. Never mutate in place after publish.
#[derive(Debug, Clone, Default)]
pub struct PathHistory {
    pub versions: Vec<VersionRecord>,
}

pub const INDEX_MAGIC_V1: &[u8] = b"ARKIDX1";
pub const INDEX_MAGIC_V2: &[u8] = b"ARKIDX2";

/// Whole namespace. `next_file_id` is the next unused inode (root uses 1).
///
/// `ino_to_path` is derived from live `file_id`s (not encoded). Hard links keep
/// the lexicographically first path (BTreeMap order), matching [`Self::find_ino`].
///
/// Maintainer: never store file_id == 0. next_file_id must always be >= 2 after
/// root is created. Rebuild_ino_map is called after every persist_index.
/// See "Inode 0" trap and "next_file_id".
#[derive(Debug, Clone, Default)]
pub struct DurableIndex {
    pub next_file_id: u64,
    pub paths: BTreeMap<PathKey, Arc<PathHistory>>,
    pub(crate) ino_to_path: HashMap<u64, PathKey>,
    /// Live directory → sorted children. Rebuilt with the inode map.
    pub(crate) live_children: HashMap<PathKey, Vec<(String, u64, FileType)>>,
}

/// Encode as `ARKIDX2`. No checksum trailer; integrity is the object's ObjectId.
///
/// Maintainer: always write V2. V1 is read-only legacy (file_id=0, no tombstone).
/// See "INDEX_MAGIC" and decode_v1. Never emit ARKIDX1.
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

/// Current on-disk format. `file_id` and tombstone are required.
///
/// Maintainer: must call rebuild_ino_map after decode. V2 is the only
/// format we write. See decode_index and "ARKIDX2".
fn decode_v2(data: &[u8]) -> Result<DurableIndex, ArkError> {
    let mut r = Reader::new(data);
    r.expect_magic(INDEX_MAGIC_V2)?;
    let mut idx = DurableIndex {
        next_file_id: r.u64()?,
        ..Default::default()
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
        idx.paths.insert(path, Arc::new(PathHistory { versions }));
    }
    r.finish()?;
    idx.rebuild_ino_map();
    Ok(idx)
}

/// Read-only legacy: live file, `file_id = 0`. Do not write V1.
///
/// Maintainer: only for reading old indexes. Decoded records have file_id=0
/// and tombstone=false. Upgrade path creates new V2 records on next write.
/// See "decode_v1" and Inode 0 handling in commit_branch.
fn decode_v1(data: &[u8]) -> Result<DurableIndex, ArkError> {
    let mut r = Reader::new(data);
    r.expect_magic(INDEX_MAGIC_V1)?;
    let mut idx = DurableIndex {
        next_file_id: r.u64()?,
        ..Default::default()
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
        idx.paths.insert(path, Arc::new(PathHistory { versions }));
    }
    r.finish()?;
    idx.rebuild_ino_map();
    Ok(idx)
}

/// FileType → ARKIDX2 u8 tag.
///
/// Maintainer: must stay in sync with decode_file_type. Unknown on decode
/// is treated as Integrity (fail closed).
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

/// ARKIDX2 u8 tag → FileType. Unknown is Integrity.
///
/// Maintainer: must round-trip with encode_file_type. Bad tag = Integrity error
/// (fail closed). See decode_v2.
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

impl VersionRecord {
    /// Same CAS ids and file identity; new cactus parent / tombstone bit.
    ///
    /// Maintainer: used for hard-link fan-out and rename/exchange tombstones.
    /// Preserves content/attrs/file_id/type. Only parent and tombstone change.
    /// See "hard link" semantics and rename_tree/exchange_tree.
    pub(crate) fn continue_as(&self, at: Timestamp, parent: Option<u32>, tombstone: bool) -> Self {
        VersionRecord {
            content_id: self.content_id,
            attrs_id: self.attrs_id,
            at,
            parent,
            tombstone,
            file_id: self.file_id,
            file_type: self.file_type,
        }
    }
}

impl PathHistory {
    /// Live = last version (caller checks tombstone). AsOf = last `at <= ts`.
    ///
    /// Maintainer: AsOf returns the last version whose timestamp is <= the
    /// query time even if later tombstones exist. Tombstone check is done by
    /// the caller (lookup_key). See "View" and "record_in_view".
    pub(crate) fn record_in_view(&self, view: View) -> Option<&VersionRecord> {
        match view {
            View::Live => self.versions.last(),
            View::AsOf(ts) => self.versions.iter().rev().find(|v| v.at <= ts),
        }
    }
}

impl DurableIndex {
    /// Greatest `at` across every version. Used to seed the clock on reopen.
    ///
    /// Maintainer: seed for TemporalCore clock on open. Used for conflict checks
    /// and --as-of. See "Logical time" and open().
    pub(crate) fn max_timestamp(&self) -> Timestamp {
        let mut max = Timestamp::ZERO;
        for hist in self.paths.values() {
            for v in &hist.versions {
                if v.at > max {
                    max = v.at;
                }
            }
        }
        max
    }

    /// Append `rec` as the newest cactus node for `path` (creates the history if needed).
    ///
    /// Maintainer: this mutates the in-memory snapshot. persist_index will
    /// encode + put + set_anchor. See commit_branch and persist_index.
    pub(crate) fn push(&mut self, path: PathKey, rec: VersionRecord) {
        let hist = self.paths.entry(path).or_default();
        Arc::make_mut(hist).versions.push(rec);
    }

    /// Index of the current last version, to store as the next record's `parent`.
    ///
    /// Maintainer: parent indexes form the per-path cactus. Used in
    /// commit_branch and tombstone. See VersionRecord::parent.
    pub(crate) fn version_parent(&self, path: &PathKey) -> Option<u32> {
        self.paths.get(path).and_then(|h| {
            let n = h.versions.len();
            (n > 0).then_some((n - 1) as u32)
        })
    }

    /// Last cactus node for `path`, including a tombstone. `None` if the path never existed.
    ///
    /// Maintainer: includes tombstones. Callers that want live data should use
    /// last_live. Used by tombstone() and conflict checks.
    pub(crate) fn last(&self, path: &PathKey) -> Option<&VersionRecord> {
        self.paths.get(path).and_then(|h| h.versions.last())
    }

    /// Last version that is not a tombstone. `NotFound` if missing or tombstoned.
    pub(crate) fn last_live(&self, path: &PathKey) -> Result<VersionRecord, ArkError> {
        let last = self
            .last(path)
            .cloned()
            .ok_or_else(|| ArkError::not_found(path.as_str()))?;
        if last.tombstone {
            Err(ArkError::not_found(path.as_str()))
        } else {
            Ok(last)
        }
    }

    /// Live paths that are `prefix` or a descendant. Used by directory rename/exchange.
    pub(crate) fn live_under(&self, prefix: &PathKey) -> Vec<PathKey> {
        self.paths
            .iter()
            .filter_map(|(p, hist)| {
                let last = hist.versions.last()?;
                if last.tombstone || !p.is_under(prefix) {
                    None
                } else {
                    Some(p.clone())
                }
            })
            .collect()
    }

    /// First live path in `view` whose `file_id` is `ino`. Hard links: BTreeMap order.
    ///
    /// Maintainer: for Live we use the derived ino_to_path map (O(1)). For AsOf
    /// we fall back to a scan in lookup_ino. Hard links pick the lex-first path.
    /// See "Inode / file_id" and lookup_ino.
    pub(crate) fn find_ino(&self, ino: u64, view: View) -> Option<PathKey> {
        for (path, hist) in &self.paths {
            if let Some(rec) = hist.record_in_view(view) {
                if !rec.tombstone && rec.file_id == ino {
                    return Some(path.clone());
                }
            }
        }
        None
    }

    /// POSIX `st_nlink` for `rec` at `path` in `view` (not stored on the attr object).
    ///
    /// Maintainer: directories start at 2 (`.` and `..`) + live subdirs.
    /// Regular files = count of live paths sharing the file_id (hard links).
    /// nlink is derived, never stored in FileAttributes. See link_count.
    pub(crate) fn nlink(&self, rec: &VersionRecord, path: &PathKey, view: View) -> u32 {
        if rec.file_type == FileType::Directory {
            let mut n = 2u32;
            for (p, hist) in &self.paths {
                if path.immediate_child(p).is_some() {
                    if let Some(r) = hist.record_in_view(view) {
                        if !r.tombstone && r.file_type == FileType::Directory {
                            n = n.saturating_add(1);
                        }
                    }
                }
            }
            n
        } else {
            let mut n = 0u32;
            for hist in self.paths.values() {
                if let Some(r) = hist.record_in_view(view) {
                    if !r.tombstone && r.file_id == rec.file_id {
                        n = n.saturating_add(1);
                    }
                }
            }
            n.max(1)
        }
    }

    /// Rebuild live inode and children maps. Call after mutating `paths`.
    ///
    /// Maintainer: must be called after every mutation that affects live
    /// entries before persist_index. Populates ino_to_path (for Live O(1)
    /// lookup) and live_children (for readdir). See "rebuild_ino_map".
    pub(crate) fn rebuild_ino_map(&mut self) {
        self.ino_to_path.clear();
        self.live_children.clear();
        for (path, hist) in &self.paths {
            if let Some(rec) = hist.record_in_view(View::Live) {
                if rec.tombstone {
                    continue;
                }
                self.ino_to_path
                    .entry(rec.file_id)
                    .or_insert_with(|| path.clone());
                if let Some(parent) = path.parent() {
                    self.live_children.entry(parent).or_default().push((
                        path.name().to_string(),
                        rec.file_id,
                        rec.file_type,
                    ));
                }
            }
        }
        for kids in self.live_children.values_mut() {
            kids.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }

    /// Count of live (non-tombstone) paths. Used by FUSE `statfs`.
    ///
    /// Maintainer: O(live paths). Does not count tombstones. See
    /// TemporalCore::live_path_count and statfs.
    pub(crate) fn live_path_count(&self) -> u64 {
        self.paths
            .values()
            .filter(|h| h.versions.last().is_some_and(|v| !v.tombstone))
            .count() as u64
    }

    /// Other live names of `file_id` (excludes `except`). Used to fan out writes.
    ///
    /// Maintainer: hard-link fan-out on write/replace_content/commit_branch.
    /// Excludes the primary path being written so it is not duplicated.
    /// See "Open-file cache is per-inode" and replace_content.
    pub(crate) fn live_siblings(&self, file_id: u64, except: &PathKey) -> Vec<PathKey> {
        self.paths
            .iter()
            .filter_map(|(p, hist)| {
                if p == except {
                    return None;
                }
                let rec = hist.record_in_view(View::Live)?;
                (!rec.tombstone && rec.file_id == file_id).then(|| p.clone())
            })
            .collect()
    }
}
