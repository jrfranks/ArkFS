//! TemporalCore — versioned path history with store-backed payloads.
//!
//! Durability is delegated exclusively to [`persistent_object_store::PersistentObjectStore`].
//! The in-memory index is a cache of the durable `temporal_index` anchor.

use arkfs_core::codec::{self, Reader, Writer};
use arkfs_core::{
    ArkError, FileAttributes, ObjectId, PathKey, QuorumPolicy, SizePolicy, Timestamp,
};
use persistent_object_store::PersistentObjectStore;
use std::collections::BTreeMap;
use std::sync::Mutex;

const INDEX_MAGIC: &[u8] = b"ARKIDX1";
const INDEX_ANCHOR: &str = "temporal_index";

/// Handle returned by lookup APIs (content is loaded via [`TemporalCore::read_content`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHandle {
    pub path: PathKey,
    pub content_id: ObjectId,
    pub attrs: FileAttributes,
    pub committed_at: Timestamp,
}

/// Delta applied on a branch commit.
#[derive(Debug, Clone)]
pub struct BranchDelta {
    pub path: String,
    pub content: Option<Vec<u8>>,
    pub attrs: Option<FileAttributes>,
    /// Logical time of this commit (caller / harness supplies).
    pub at: Timestamp,
}

#[derive(Debug, Clone)]
struct VersionRecord {
    content_id: ObjectId,
    attrs_id: ObjectId,
    at: Timestamp,
    /// Index of the parent version in this path's history (cactus spine).
    parent: Option<u32>,
}

#[derive(Debug, Clone, Default)]
struct PathHistory {
    versions: Vec<VersionRecord>,
}

#[derive(Debug, Clone, Default)]
struct DurableIndex {
    next_file_id: u64,
    paths: BTreeMap<PathKey, PathHistory>,
}

/// Store-backed path history. Commits are serialized; lookups do not hold I/O locks.
pub struct TemporalCore {
    store: PersistentObjectStore,
    index: Mutex<DurableIndex>,
    commit: Mutex<()>,
    quorum: QuorumPolicy,
}

impl TemporalCore {
    pub fn open(store: PersistentObjectStore, quorum: QuorumPolicy) -> Result<Self, ArkError> {
        let index = match store.get_anchor(INDEX_ANCHOR)? {
            Some(id) => decode_index(&store.get(&id)?)?,
            None => DurableIndex {
                next_file_id: 1,
                paths: BTreeMap::new(),
            },
        };
        Ok(TemporalCore {
            store,
            index: Mutex::new(index),
            commit: Mutex::new(()),
            quorum,
        })
    }

    pub fn lookup_current(&self, path: &str) -> Result<FileHandle, ArkError> {
        let rec = {
            let index = self.index.lock().unwrap();
            let hist = index
                .paths
                .get(&PathKey::new(path))
                .ok_or_else(|| ArkError::not_found(path))?;
            hist.versions
                .last()
                .cloned()
                .ok_or_else(|| ArkError::not_found(path))?
        };
        self.materialize(path, &rec)
    }

    pub fn lookup_at_timestamp(&self, path: &str, ts: Timestamp) -> Result<FileHandle, ArkError> {
        let rec = {
            let index = self.index.lock().unwrap();
            let hist = index
                .paths
                .get(&PathKey::new(path))
                .ok_or_else(|| ArkError::not_found(path))?;
            hist.versions
                .iter()
                .rev()
                .find(|v| v.at <= ts)
                .cloned()
                .ok_or_else(|| ArkError::not_found(format!("{path} at {ts:?}")))?
        };
        self.materialize(path, &rec)
    }

    pub fn read_content(&self, handle: &FileHandle) -> Result<Vec<u8>, ArkError> {
        self.store.get(&handle.content_id)
    }

    pub fn commit_branch(&self, delta: BranchDelta) -> Result<(), ArkError> {
        let _commit = self.commit.lock().unwrap();
        let BranchDelta {
            path,
            content,
            attrs,
            at,
        } = delta;
        let path = PathKey::new(path);
        let snapshot = self.index.lock().unwrap().clone();

        if let Some(last) = snapshot.paths.get(&path).and_then(|h| h.versions.last()) {
            if at <= last.at {
                return Err(ArkError::conflict("commit timestamp before latest version"));
            }
        }

        let parent = snapshot.paths.get(&path).and_then(|h| {
            let n = h.versions.len();
            (n > 0).then_some((n - 1) as u32)
        });

        let (content, mut attrs, next_file_id) = if let Some(hist) = snapshot.paths.get(&path) {
            let last = hist
                .versions
                .last()
                .ok_or_else(|| ArkError::not_found(path.as_str()))?;
            let content = match content {
                Some(c) => c,
                None => self.store.get(&last.content_id)?,
            };
            let attrs = match attrs {
                Some(a) => a,
                None => codec::decode_attrs(&self.store.get(&last.attrs_id)?)?,
            };
            (content, attrs, snapshot.next_file_id)
        } else {
            let content = content.unwrap_or_default();
            let mut attrs = attrs.unwrap_or_default();
            let mut next = snapshot.next_file_id;
            if attrs.file_id == 0 {
                attrs.file_id = next;
                next += 1;
            } else if attrs.file_id >= next {
                next = attrs.file_id + 1;
            }
            (content, attrs, next)
        };

        attrs.set_logical_size(content.len() as u64, SizePolicy::Logical);
        attrs.touch_change(at.to_timespec());

        let content_id = self.store.put(&content, self.quorum)?;
        if let Some(s) = attrs.primary_stream_mut() {
            s.content_id = Some(*content_id.as_bytes());
        }
        let attrs_id = self.store.put(&codec::encode_attrs(&attrs), self.quorum)?;

        let rec = VersionRecord {
            content_id,
            attrs_id,
            at,
            parent,
        };
        let mut new_index = snapshot;
        new_index.next_file_id = next_file_id;
        new_index.paths.entry(path).or_default().versions.push(rec);

        let index_id = self.store.put(&encode_index(&new_index), self.quorum)?;
        self.store
            .set_anchor(INDEX_ANCHOR, &index_id, self.quorum)?;
        *self.index.lock().unwrap() = new_index;
        Ok(())
    }

    /// Apply attribute-only patch via full-record RMW (preserves foreign protocol fields).
    pub fn commit_attrs(
        &self,
        path: &str,
        mutator: impl FnOnce(&mut FileAttributes),
        at: Timestamp,
    ) -> Result<(), ArkError> {
        let current = self.lookup_current(path)?;
        let mut attrs = current.attrs;
        mutator(&mut attrs);
        self.commit_branch(BranchDelta {
            path: path.into(),
            content: None,
            attrs: Some(attrs),
            at,
        })
    }

    fn materialize(&self, path: &str, rec: &VersionRecord) -> Result<FileHandle, ArkError> {
        let attrs = codec::decode_attrs(&self.store.get(&rec.attrs_id)?)?;
        Ok(FileHandle {
            path: PathKey::new(path),
            content_id: rec.content_id,
            attrs,
            committed_at: rec.at,
        })
    }

    pub fn store(&self) -> &PersistentObjectStore {
        &self.store
    }

    #[cfg(test)]
    fn parents(&self, path: &str) -> Vec<Option<u32>> {
        self.index
            .lock()
            .unwrap()
            .paths
            .get(&PathKey::new(path))
            .map(|h| h.versions.iter().map(|v| v.parent).collect())
            .unwrap_or_default()
    }
}

fn encode_index(idx: &DurableIndex) -> Vec<u8> {
    let mut w = Writer::with_magic(INDEX_MAGIC);
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
        }
    }
    w.into_inner()
}

fn decode_index(data: &[u8]) -> Result<DurableIndex, ArkError> {
    let mut r = Reader::new(data);
    r.expect_magic(INDEX_MAGIC)?;
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
            });
        }
        idx.paths.insert(path, PathHistory { versions });
    }
    r.finish()?;
    Ok(idx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arkfs_core::attr_map::{merge_from_fuse, FuseSetAttr};
    use arkfs_core::{DosFlags, FileType, MacOsFlags, NamedStream, Timespec};
    use persistent_object_store::open_local_quorum_store;

    fn core() -> (tempfile::TempDir, TemporalCore) {
        let dir = tempfile::tempdir().unwrap();
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        (
            dir,
            TemporalCore::open(store, QuorumPolicy::Quorum(1)).unwrap(),
        )
    }

    #[test]
    fn commit_and_lookup_current() {
        let (_d, tc) = core();
        let t0 = Timestamp::new(1, 1000);
        tc.commit_branch(BranchDelta {
            path: "/notes.txt".into(),
            content: Some(b"v1".to_vec()),
            attrs: None,
            at: t0,
        })
        .unwrap();
        let h = tc.lookup_current("/notes.txt").unwrap();
        assert_eq!(tc.read_content(&h).unwrap(), b"v1");
        assert_eq!(h.attrs.logical_size, 2);
    }

    #[test]
    fn historical_lookup_never_delete() {
        let (_d, tc) = core();
        let t0 = Timestamp::new(1, 1000);
        let t1 = Timestamp::new(2, 2000);
        tc.commit_branch(BranchDelta {
            path: "/f".into(),
            content: Some(b"old".to_vec()),
            attrs: None,
            at: t0,
        })
        .unwrap();
        tc.commit_branch(BranchDelta {
            path: "/f".into(),
            content: Some(b"new".to_vec()),
            attrs: None,
            at: t1,
        })
        .unwrap();
        assert_eq!(
            tc.read_content(&tc.lookup_at_timestamp("/f", t0).unwrap())
                .unwrap(),
            b"old"
        );
        assert_eq!(
            tc.read_content(&tc.lookup_at_timestamp("/f", t1).unwrap())
                .unwrap(),
            b"new"
        );
        assert_eq!(
            tc.read_content(&tc.lookup_current("/f").unwrap()).unwrap(),
            b"new"
        );
        assert_eq!(tc.parents("/f"), vec![None, Some(0)]);
    }

    #[test]
    fn attr_historical_snapshot() {
        let (_d, tc) = core();
        let mut a0 = FileAttributes::new_file(0, 0o644);
        a0.dos.hidden = true;
        a0.dos.archive = true;
        a0.xattrs
            .insert("com.apple.quarantine".into(), b"q".to_vec());
        let t0 = Timestamp::new(1, 1);
        tc.commit_branch(BranchDelta {
            path: "/a".into(),
            content: Some(b"x".to_vec()),
            attrs: Some(a0.clone()),
            at: t0,
        })
        .unwrap();

        let t1 = Timestamp::new(2, 2);
        tc.commit_attrs(
            "/a",
            |attrs| {
                attrs.dos.hidden = false;
                attrs.mode = 0o600;
            },
            t1,
        )
        .unwrap();

        let past = tc.lookup_at_timestamp("/a", t0).unwrap();
        assert!(past.attrs.dos.hidden);
        assert!(past.attrs.dos.archive);
        assert_eq!(past.attrs.mode, 0o644);
        assert!(past.attrs.xattrs.contains_key("com.apple.quarantine"));

        let cur = tc.lookup_current("/a").unwrap();
        assert!(!cur.attrs.dos.hidden);
        assert!(cur.attrs.dos.archive);
        assert_eq!(cur.attrs.mode, 0o600);
        assert!(cur.attrs.xattrs.contains_key("com.apple.quarantine"));
    }

    #[test]
    fn fuse_partial_does_not_clobber_smb_macos() {
        let (_d, tc) = core();
        let mut a = FileAttributes::new_file(0, 0o644);
        a.dos = DosFlags {
            hidden: true,
            system: true,
            archive: true,
            ..Default::default()
        };
        a.macos = MacOsFlags {
            uf_immutable: true,
            ..Default::default()
        };
        a.dead_props.insert("{urn:ex}color".into(), "blue".into());
        a.streams.push(NamedStream {
            name: NamedStream::RESOURCE_FORK.into(),
            size: 8,
            content_id: None,
        });
        let t0 = Timestamp::new(1, 1);
        tc.commit_branch(BranchDelta {
            path: "/m".into(),
            content: Some(b"data".to_vec()),
            attrs: Some(a),
            at: t0,
        })
        .unwrap();

        let t1 = Timestamp::new(2, 2);
        tc.commit_attrs(
            "/m",
            |attrs| {
                merge_from_fuse(
                    attrs,
                    &FuseSetAttr {
                        mode: Some(0o700),
                        uid: Some(99),
                        gid: None,
                        size: None,
                        atime: None,
                        mtime: None,
                        ctime: None,
                    },
                    Timespec::new(2, 0),
                );
            },
            t1,
        )
        .unwrap();

        let h = tc.lookup_current("/m").unwrap();
        assert_eq!(h.attrs.mode, 0o700);
        assert_eq!(h.attrs.uid, 99);
        assert!(h.attrs.dos.hidden);
        assert!(h.attrs.dos.system);
        assert!(h.attrs.dos.archive);
        assert!(h.attrs.macos.uf_immutable);
        assert_eq!(
            h.attrs.dead_props.get("{urn:ex}color").map(String::as_str),
            Some("blue")
        );
        assert!(h
            .attrs
            .streams
            .iter()
            .any(|s| s.name == NamedStream::RESOURCE_FORK));
    }

    #[test]
    fn directory_roundtrip() {
        let (_d, tc) = core();
        let t0 = Timestamp::new(1, 1);
        tc.commit_branch(BranchDelta {
            path: "/dir".into(),
            content: Some(Vec::new()),
            attrs: Some(FileAttributes::new_dir(0, 0o755)),
            at: t0,
        })
        .unwrap();
        let h = tc.lookup_current("/dir").unwrap();
        assert_eq!(h.attrs.file_type, FileType::Directory);
        assert_eq!(h.attrs.nlink, 2);
        assert!(h.attrs.dos.directory);
        assert!(h.attrs.streams.is_empty());
        assert_eq!(h.attrs.mode, 0o755);
    }

    #[test]
    fn conflict_before_write_keeps_latest() {
        let (_d, tc) = core();
        tc.commit_branch(BranchDelta {
            path: "/c".into(),
            content: Some(b"new".to_vec()),
            attrs: None,
            at: Timestamp::new(2, 2),
        })
        .unwrap();
        let err = tc
            .commit_branch(BranchDelta {
                path: "/c".into(),
                content: Some(b"old".to_vec()),
                attrs: None,
                at: Timestamp::new(1, 1),
            })
            .unwrap_err();
        assert!(matches!(err, ArkError::Conflict { .. }));
        assert_eq!(
            tc.read_content(&tc.lookup_current("/c").unwrap()).unwrap(),
            b"new"
        );
    }

    #[test]
    fn index_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
            let tc = TemporalCore::open(store, QuorumPolicy::Quorum(1)).unwrap();
            tc.commit_branch(BranchDelta {
                path: "/p".into(),
                content: Some(b"persist".to_vec()),
                attrs: None,
                at: Timestamp::new(1, 1),
            })
            .unwrap();
            tc.commit_branch(BranchDelta {
                path: "/p".into(),
                content: Some(b"persist2".to_vec()),
                attrs: None,
                at: Timestamp::new(2, 2),
            })
            .unwrap();
        }
        let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
        let tc = TemporalCore::open(store, QuorumPolicy::Quorum(1)).unwrap();
        let h = tc.lookup_current("/p").unwrap();
        assert_eq!(tc.read_content(&h).unwrap(), b"persist2");
        assert_eq!(h.attrs.file_id, 1);
        assert_eq!(tc.parents("/p"), vec![None, Some(0)]);
    }
}
