//! Communications / IPC suite for [`persistent_object_store`].
//!
//! Oracle: `docs/conformance/ipc.md` §7. This file is that suite.

use arkfs_core::{ArkError, ObjectId, QuorumPolicy};
#[allow(unused_imports)]
use arkfs_test_review::{review_assert as assert, review_eq as assert_eq, review_ne as assert_ne};
use persistent_object_store::{
    open_isolated_store, open_local_quorum_store, LocalQuorum, NoPeers, PersistentObjectStore,
    ReplicationBackend,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Backend whose `replicate_*` always fail; used to prove no publish on `Err`.
struct DenyReplicate;

impl ReplicationBackend for DenyReplicate {
    fn replicate_object(&self, _: &ObjectId, _: &[u8]) -> Result<u32, ArkError> {
        Err(ArkError::invalid_argument("denied"))
    }
    fn replicate_anchor(&self, _: &str, _: &[u8; 32]) -> Result<u32, ArkError> {
        Err(ArkError::invalid_argument("denied"))
    }
    fn always_on_count(&self) -> u32 {
        0
    }
}

/// Same name list as [`open_local_quorum_store`]: `[primary_label, remotes…]`.
fn cluster(base: &Path, names: &[&str]) -> (LocalQuorum, PersistentObjectStore) {
    let q = LocalQuorum::remotes_under(base, names).unwrap();
    let store = PersistentObjectStore::open(base.join("primary"), Box::new(q.clone())).unwrap();
    (q, store)
}

fn replica_obj(base: &Path, node: &str, id: &ObjectId) -> PathBuf {
    base.join("replicas")
        .join(node)
        .join("objects")
        .join(format!("{}.obj", id.to_hex()))
}

fn replica_anchor(base: &Path, node: &str, name: &str) -> PathBuf {
    base.join("replicas").join(node).join("anchors").join(name)
}

fn tmp_leftovers(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !dir.exists() {
        return out;
    }
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) == Some("tmp") {
            out.push(p);
        }
    }
    out
}

/// NoPeers replicate_* return 0 and always_on_count is 0.
#[test]
fn no_peers_replicate_returns_zero() {
    let _g = arkfs_test_review::guard();
    let b = NoPeers;
    assert_eq!(
        b.replicate_object(&ObjectId::from_bytes(b"x"), b"x")
            .unwrap(),
        0
    );
    assert_eq!(b.replicate_anchor("temporal_index", &[0u8; 32]).unwrap(), 0);
    assert_eq!(b.always_on_count(), 0);
}

/// Isolated store: primary root, OwnerOnly and AllAlwaysOn, no replicas/.
#[test]
fn open_isolated_store_layout() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let store = open_isolated_store(dir.path()).unwrap();
    assert!(store.root().ends_with("primary"));
    let id = store.put(b"blob", QuorumPolicy::OwnerOnly).unwrap();
    store
        .set_anchor("temporal_index", &id, QuorumPolicy::AllAlwaysOn)
        .unwrap();
    assert!(store.root().join("objects").is_dir());
    assert!(store.root().join("anchors").is_dir());
    assert!(!dir.path().join("replicas").exists());
    assert!(store
        .object_path(&id)
        .starts_with(dir.path().join("primary")));
}

/// NoPeers + Quorum(2) fails closed; no replicas/.
#[test]
fn isolated_quorum_two_fails_closed() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let store = open_isolated_store(dir.path()).unwrap();
    let err = store.put(b"x", QuorumPolicy::n(2)).unwrap_err();
    assert!(matches!(err, ArkError::Quorum { got: 1, need: 2 }));
    assert!(!store.object_path(&ObjectId::from_bytes(b"x")).exists());
    assert!(!dir.path().join("replicas").exists());
    assert!(tmp_leftovers(&store.root().join("objects")).is_empty());
}

/// Quorum(1) on NoPeers is the local write alone (need is not always_on).
#[test]
fn quorum_one_succeeds_isolated() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let store = open_isolated_store(dir.path()).unwrap();
    let id = store.put(b"q1", QuorumPolicy::n(1)).unwrap();
    assert_eq!(store.get(&id).unwrap(), b"q1");
}

/// LocalQuorum copies object and anchor bytes into replica directories.
#[test]
fn local_quorum_copies_object_and_anchor() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
    let data = b"replica-bytes";
    let id = store.put(data, QuorumPolicy::n(2)).unwrap();
    store
        .set_anchor("temporal_index", &id, QuorumPolicy::n(2))
        .unwrap();
    let robj = replica_obj(dir.path(), "n1", &id);
    let ranch = replica_anchor(dir.path(), "n1", "temporal_index");
    assert!(robj.exists());
    assert_eq!(fs::read(&robj).unwrap(), data);
    assert!(ranch.exists());
    assert_eq!(fs::read(&ranch).unwrap(), id.as_bytes().as_slice());
    assert!(tmp_leftovers(robj.parent().unwrap()).is_empty());
    assert!(tmp_leftovers(ranch.parent().unwrap()).is_empty());
}

/// Lost remotes are skipped; restore makes that replica count again.
#[test]
fn lose_node_skips_replica_restore_counts_again() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let (q, store) = cluster(dir.path(), &["n0", "n1", "n2"]);
    let a = store.put(b"aaa", QuorumPolicy::n(2)).unwrap();
    assert!(replica_obj(dir.path(), "n1", &a).exists());
    assert!(replica_obj(dir.path(), "n2", &a).exists());

    q.lose_node("n1");
    q.lose_node("n2");
    assert_eq!(q.always_on_count(), 0);
    assert_eq!(
        q.replicate_object(&ObjectId::from_bytes(b"z"), b"z")
            .unwrap(),
        0
    );
    let err = store.put(b"bbb", QuorumPolicy::n(2)).unwrap_err();
    assert!(matches!(err, ArkError::Quorum { got: 1, need: 2 }));
    let b = ObjectId::from_bytes(b"bbb");
    assert!(!store.object_path(&b).exists());
    assert!(!replica_obj(dir.path(), "n1", &b).exists());
    assert!(!replica_obj(dir.path(), "n2", &b).exists());
    assert!(tmp_leftovers(&store.root().join("objects")).is_empty());

    q.restore_node("n1");
    assert_eq!(q.always_on_count(), 1);
    let c = store.put(b"ccc", QuorumPolicy::n(2)).unwrap();
    assert!(replica_obj(dir.path(), "n1", &c).exists());
    assert!(!replica_obj(dir.path(), "n2", &c).exists());
}

/// AllAlwaysOn required acks drop when remotes are marked lost.
#[test]
fn all_always_on_tracks_lost_set() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let (q, store) = cluster(dir.path(), &["n0", "n1", "n2"]);
    assert_eq!(q.always_on_count(), 2);
    let full = store.put(b"full", QuorumPolicy::AllAlwaysOn).unwrap();
    assert!(replica_obj(dir.path(), "n1", &full).exists());
    assert!(replica_obj(dir.path(), "n2", &full).exists());

    q.lose_node("n2");
    assert_eq!(q.always_on_count(), 1);
    let one = store.put(b"one-remote", QuorumPolicy::AllAlwaysOn).unwrap();
    assert!(replica_obj(dir.path(), "n1", &one).exists());
    assert!(!replica_obj(dir.path(), "n2", &one).exists());

    q.lose_node("n1");
    assert_eq!(q.always_on_count(), 0);
    let local = store.put(b"local-only", QuorumPolicy::AllAlwaysOn).unwrap();
    assert!(!replica_obj(dir.path(), "n1", &local).exists());
    assert!(!replica_obj(dir.path(), "n2", &local).exists());
}

/// Quorum(n) is not reduced to reachable count: 1+1 acks fail Quorum(3).
/// Replicate still ran on the live remote before require_quorum.
#[test]
fn quorum_fails_when_partial_remotes_below_n() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let (q, store) = cluster(dir.path(), &["n0", "n1", "n2"]);
    q.lose_node("n2");
    let err = store.put(b"partial", QuorumPolicy::n(3)).unwrap_err();
    assert!(matches!(err, ArkError::Quorum { got: 2, need: 3 }));
    let id = ObjectId::from_bytes(b"partial");
    assert!(!store.object_path(&id).exists());
    assert!(replica_obj(dir.path(), "n1", &id).exists());
    assert!(!replica_obj(dir.path(), "n2", &id).exists());
    assert!(tmp_leftovers(&store.root().join("objects")).is_empty());
}

/// open_local_quorum_store: empty error; one name is zero remotes; skip(1) under replicas/.
#[test]
fn open_local_quorum_store_arity() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        open_local_quorum_store(dir.path(), &[]),
        Err(ArkError::InvalidArgument { .. })
    ));

    let one = open_local_quorum_store(dir.path().join("one"), &["owner"]).unwrap();
    one.put(b"solo", QuorumPolicy::OwnerOnly).unwrap();
    assert!(!dir.path().join("one/replicas").exists());
    assert!(one.root().ends_with("primary"));

    let two = open_local_quorum_store(dir.path().join("two"), &["n0", "n1"]).unwrap();
    let id = two.put(b"pair", QuorumPolicy::n(2)).unwrap();
    assert!(replica_obj(&dir.path().join("two"), "n1", &id).exists());
    assert!(!dir.path().join("two/replicas/n0").exists());
}

/// Existing put still requires quorum; catch-up after restore copies remotes.
#[test]
fn existing_put_still_requires_quorum() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let (q, store) = cluster(dir.path(), &["n0", "n1", "n2"]);
    let data = b"catch-up";
    let id = store.put(data, QuorumPolicy::n(2)).unwrap();
    let r1 = replica_obj(dir.path(), "n1", &id);
    assert!(r1.exists());
    fs::remove_file(&r1).unwrap();

    q.lose_node("n1");
    q.lose_node("n2");
    let err = store.put(data, QuorumPolicy::n(2)).unwrap_err();
    assert!(matches!(err, ArkError::Quorum { got: 1, need: 2 }));
    assert_eq!(store.get(&id).unwrap(), data);
    assert!(!r1.exists());

    q.restore_node("n1");
    q.restore_node("n2");
    let again = store.put(data, QuorumPolicy::n(2)).unwrap();
    assert_eq!(again, id);
    assert!(r1.exists());
    assert_eq!(fs::read(&r1).unwrap(), data);
}

/// Existing put of a corrupt primary rewrites it; quorum failure leaves it corrupt.
#[test]
fn existing_put_corrupt_primary_is_rewritten() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let (q, store) = cluster(dir.path(), &["n0", "n1", "n2"]);
    let data = b"payload";
    let id = store.put(data, QuorumPolicy::n(2)).unwrap();
    let path = store.object_path(&id);
    let mut buf = fs::read(&path).unwrap();
    buf[0] ^= 1;
    fs::write(&path, &buf).unwrap();

    q.lose_node("n1");
    q.lose_node("n2");
    let err = store.put(data, QuorumPolicy::n(2)).unwrap_err();
    assert!(matches!(err, ArkError::Quorum { got: 1, need: 2 }));
    assert!(matches!(store.get(&id), Err(ArkError::Integrity { .. })));

    q.restore_node("n1");
    q.restore_node("n2");
    let again = store.put(data, QuorumPolicy::n(2)).unwrap();
    assert_eq!(again, id);
    assert_eq!(store.get(&id).unwrap(), data);
}

/// Existing put: replicate Err does not unpublish the primary name.
#[test]
fn existing_put_replicate_err_keeps_primary() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let store = open_isolated_store(dir.path()).unwrap();
    let data = b"keep-me";
    let id = store.put(data, QuorumPolicy::OwnerOnly).unwrap();
    drop(store);
    let store =
        PersistentObjectStore::open(dir.path().join("primary"), Box::new(DenyReplicate)).unwrap();
    let err = store.put(data, QuorumPolicy::OwnerOnly).unwrap_err();
    assert!(matches!(err, ArkError::InvalidArgument { .. }));
    assert!(store.object_path(&id).exists());
    assert_eq!(store.get(&id).unwrap(), data);
}

/// set_anchor overwrite still requires quorum; failed overwrite leaves prior bytes.
#[test]
fn set_anchor_overwrite_still_requires_quorum() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let (q, store) = cluster(dir.path(), &["n0", "n1", "n2"]);
    let first = store.put(b"idx-1", QuorumPolicy::n(2)).unwrap();
    store
        .set_anchor("temporal_index", &first, QuorumPolicy::n(2))
        .unwrap();
    assert_eq!(store.get_anchor("temporal_index").unwrap(), Some(first));

    q.lose_node("n1");
    q.lose_node("n2");
    let second = store.put(b"idx-2", QuorumPolicy::OwnerOnly).unwrap();
    let err = store
        .set_anchor("temporal_index", &second, QuorumPolicy::n(2))
        .unwrap_err();
    assert!(matches!(err, ArkError::Quorum { got: 1, need: 2 }));
    assert_eq!(store.get_anchor("temporal_index").unwrap(), Some(first));
    assert!(tmp_leftovers(&store.root().join("anchors")).is_empty());
}

/// Invalid anchor names fail before replica I/O and create no files.
#[test]
fn invalid_anchor_name_is_invalid_argument() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let store = open_local_quorum_store(dir.path(), &["n0", "n1"]).unwrap();
    let id = store.put(b"x", QuorumPolicy::n(2)).unwrap();
    for name in ["", "a/b", "has.dot", "has space", "-dash"] {
        let err = store
            .set_anchor(name, &id, QuorumPolicy::OwnerOnly)
            .unwrap_err();
        assert!(
            matches!(err, ArkError::InvalidArgument { .. }),
            "{name}: {err}"
        );
        // Empty name joins to the anchors directory itself; require no file.
        assert!(!store.anchor_path(name).is_file(), "{name}");
        assert!(!replica_anchor(dir.path(), "n1", name).is_file(), "{name}");
    }
}

/// replicate_* Err must not publish object or anchor names, and leaves no .tmp.
#[test]
fn replicate_err_does_not_publish() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let store =
        PersistentObjectStore::open(dir.path().join("primary"), Box::new(DenyReplicate)).unwrap();
    let err = store.put(b"nope", QuorumPolicy::OwnerOnly).unwrap_err();
    assert!(matches!(err, ArkError::InvalidArgument { .. }));
    let id = ObjectId::from_bytes(b"nope");
    assert!(!store.object_path(&id).exists());
    assert!(tmp_leftovers(&store.root().join("objects")).is_empty());

    let err = store
        .set_anchor("temporal_index", &id, QuorumPolicy::OwnerOnly)
        .unwrap_err();
    assert!(matches!(err, ArkError::InvalidArgument { .. }));
    assert!(!store.anchor_path("temporal_index").exists());
    assert!(tmp_leftovers(&store.root().join("anchors")).is_empty());
}

/// Losing an unknown node is a no-op; restore of a never-lost node is a no-op.
#[test]
fn lose_unknown_and_restore_never_lost_are_nops() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let (q, store) = cluster(dir.path(), &["n0", "n1"]);
    assert_eq!(q.always_on_count(), 1);
    q.lose_node("no-such");
    assert_eq!(q.always_on_count(), 1);
    q.restore_node("n1");
    assert_eq!(q.always_on_count(), 1);
    store.put(b"ok", QuorumPolicy::n(2)).unwrap();
}

/// Double lose_node of the same id does not underflow always_on_count.
#[test]
fn double_lose_same_node() {
    let _g = arkfs_test_review::guard();
    let dir = tempfile::tempdir().unwrap();
    let (q, store) = cluster(dir.path(), &["n0", "n1", "n2"]);
    q.lose_node("n1");
    q.lose_node("n1");
    assert_eq!(q.always_on_count(), 1);
    store.put(b"still", QuorumPolicy::n(2)).unwrap();
}
