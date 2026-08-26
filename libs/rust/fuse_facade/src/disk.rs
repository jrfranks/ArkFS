//! On-disk store inspector used by the live FUSE scaffold.
//!
//! Reads `data/primary/{objects,anchors/temporal_index}` independently of the
//! mounted process so a test can assert CAS after each kernel op. Panics on
//! invariant violations (wrong filename hash, missing content object) — this
//! is test-only code, not the production get path.
//!
//! [`DiskView::assert_never_deleted_objects`] is the never-delete check:
//! object ids present before an unlink must still exist after.

use arkfs_core::codec::decode_attrs;
use arkfs_core::{FileAttributes, FileType, ObjectId};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use temporal_core::{load_index, PersistedIndex, INDEX_ANCHOR};

/// One live path as reconstructed from the last index version + CAS objects.
#[derive(Debug, Clone)]
pub struct LiveNode {
    pub file_type: FileType,
    pub file_id: u64,
    pub content: Vec<u8>,
    pub attrs: FileAttributes,
    pub versions: usize,
}

/// Snapshot of primary store + decoded index. Built by [`inspect`].
#[derive(Debug, Clone)]
pub struct DiskView {
    pub object_hex: BTreeSet<String>,
    pub stray_tmp: Vec<String>,
    pub live: BTreeMap<String, LiveNode>,
    pub tombstoned: BTreeSet<String>,
    pub index: PersistedIndex,
}

/// Read `data_dir/primary` as a [`DiskView`]. Caller should `sync()` first.
pub fn inspect(data_dir: &Path) -> DiskView {
    let primary = data_dir.join("primary");
    let objects = primary.join("objects");
    let mut object_hex = BTreeSet::new();
    let mut stray_tmp = Vec::new();
    if objects.is_dir() {
        for e in fs::read_dir(&objects).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                stray_tmp.push(name);
                continue;
            }
            if !name.ends_with(".obj") {
                continue;
            }
            let stem = name.trim_end_matches(".obj");
            let bytes = fs::read(e.path()).unwrap();
            let id = ObjectId::from_bytes(&bytes);
            assert_eq!(
                id.to_hex(),
                stem,
                "CAS filename must match blake3(payload) for {name}"
            );
            object_hex.insert(stem.to_string());
        }
    }
    let anchor = fs::read(primary.join("anchors").join(INDEX_ANCHOR)).unwrap();
    assert_eq!(anchor.len(), 32, "temporal_index anchor is 32 bytes");
    let mut idb = [0u8; 32];
    idb.copy_from_slice(&anchor);
    let index_id = ObjectId(idb);
    let index_bytes = fs::read(objects.join(format!("{}.obj", index_id.to_hex()))).unwrap();
    let index = load_index(&index_bytes).unwrap();

    let mut live = BTreeMap::new();
    let mut tombstoned = BTreeSet::new();
    for (path, hist) in &index.paths {
        let last = hist.versions.last().expect("path history not empty");
        if last.tombstone {
            tombstoned.insert(path.as_str().to_string());
            continue;
        }
        let content = fs::read(objects.join(format!("{}.obj", last.content_id.to_hex()))).unwrap();
        let attr_bytes = fs::read(objects.join(format!("{}.obj", last.attrs_id.to_hex()))).unwrap();
        let attrs = decode_attrs(&attr_bytes).unwrap();
        assert!(
            object_hex.contains(&last.content_id.to_hex()),
            "missing content object for {}",
            path.as_str()
        );
        assert!(
            object_hex.contains(&last.attrs_id.to_hex()),
            "missing attrs object for {}",
            path.as_str()
        );
        live.insert(
            path.as_str().to_string(),
            LiveNode {
                file_type: last.file_type,
                file_id: last.file_id,
                content,
                attrs,
                versions: hist.versions.len(),
            },
        );
    }

    DiskView {
        object_hex,
        stray_tmp,
        live,
        tombstoned,
        index,
    }
}

impl DiskView {
    /// No stray .tmp staging files; root `/` is live.
    pub fn assert_clean(&self) {
        assert!(
            self.stray_tmp.is_empty(),
            "unpublished tmp files: {:?}",
            self.stray_tmp
        );
        assert!(self.live.contains_key("/"), "root must exist on disk");
    }

    /// Live path is a regular file whose CAS bytes equal `body`.
    pub fn assert_file(&self, path: &str, body: &[u8]) {
        let n = self
            .live
            .get(path)
            .unwrap_or_else(|| panic!("missing live {path}"));
        assert_eq!(n.file_type, FileType::File, "{path} type");
        assert_eq!(n.content, body, "{path} content");
        assert_eq!(n.attrs.logical_size, body.len() as u64);
    }

    /// Live path is a FIFO (mknod S_IFIFO). Content is empty.
    pub fn assert_fifo(&self, path: &str) {
        let n = self
            .live
            .get(path)
            .unwrap_or_else(|| panic!("missing live {path}"));
        assert_eq!(n.file_type, FileType::Fifo, "{path} type");
        assert!(n.content.is_empty(), "{path} fifo content");
    }

    /// Live path is a directory.
    pub fn assert_dir(&self, path: &str) {
        let n = self
            .live
            .get(path)
            .unwrap_or_else(|| panic!("missing live {path}"));
        assert_eq!(n.file_type, FileType::Directory, "{path} type");
    }

    /// Live path is a symlink whose stored target equals `target`.
    pub fn assert_symlink(&self, path: &str, target: &str) {
        let n = self
            .live
            .get(path)
            .unwrap_or_else(|| panic!("missing live {path}"));
        assert_eq!(n.file_type, FileType::Symlink, "{path} type");
        assert_eq!(n.attrs.symlink_target.as_deref(), Some(target));
    }

    /// Path is tombstoned in the index and absent from the live map.
    pub fn assert_tombstone(&self, path: &str) {
        assert!(
            self.tombstoned.contains(path),
            "{path} should be tombstoned, live={:?} tomb={:?}",
            self.live.keys().collect::<Vec<_>>(),
            self.tombstoned
        );
        assert!(!self.live.contains_key(path));
    }

    /// Every object id in `before` is still on disk (never-delete).
    pub fn assert_never_deleted_objects(&self, before: &BTreeSet<String>) {
        for id in before {
            assert!(
                self.object_hex.contains(id),
                "never-delete violated: object {id} disappeared"
            );
        }
    }
}
