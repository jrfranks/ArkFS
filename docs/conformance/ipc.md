# ArkFS IPC library conformance

**Status: Complete for the implemented IPC library (Phase 0).** SPEC.md
InternalChannel (`connect`, `push_delta`, `request_historical`,
`gossip_digest`) is **not implemented** and is **not specified** here. This
file is a test oracle for the library that exists. It is not a test plan for
Phase 1.

Related: [SPEC.md](../../SPEC.md) (roadmap), [phase0.md](../phase0.md),
[maintainer.md](../maintainer.md), [fuse-abi.md](fuse-abi.md) (kernel ABI
inventory, not a test oracle), [posix-xsh.md](posix-xsh.md) (XSH inventory,
not a test oracle). ArkFS FUSE test reachability is
[fuse-conformance.md](../fuse-conformance.md).

Citation keys (`[S1]` …) are under [Sources](#sources).

---

## 1. Identity: what the IPC library is

The ArkFS IPC library in this tree is the **replication hook** used by
`PersistentObjectStore` to obtain remote durability acknowledgments, plus
the quorum arithmetic that interprets those acks.

| Piece | Path | Role |
| --- | --- | --- |
| `ReplicationBackend` | `libs/rust/persistent_object_store/src/lib.rs` | Trait: `replicate_object`, `replicate_anchor`, `always_on_count` |
| `NoPeers` | same | Isolated backend |
| `LocalQuorum` | same | In-process replica directories; `Clone` shares lost-set; `lose_node` / `restore_node` |
| `LocalQuorum::remotes_under` | same | `names[0]` is primary label; `names[1..]` at `base/replicas/<name>` |
| `open_isolated_store` | same | `base/primary` + `NoPeers` |
| `open_local_quorum_store` | same | `base/primary` + `LocalQuorum::remotes_under` |
| `PersistentObjectStore::{put, set_anchor, require_quorum}` | same | Callers that wait on the backend then publish |
| `QuorumPolicy` | `libs/rust/arkfs_core/src/quorum.rs` | `n(k)` / `AllAlwaysOn` / `OwnerOnly` (`k >= 1`) |

There is **no network socket, QUIC session, handshake, or message codec**.
`LocalQuorum` copies bytes into extra directories with `write_fsync`.
`NoPeers` does no I/O. `[S1]` `[S4]`

Phase 0 FUSE mounts **must** use `open_isolated_store` + `OwnerOnly`.
`LocalQuorum` is the cluster-**simulation** backend for store tests, not the
FUSE production path. `[S6]` `[S7]`

---

## 2. Not this library

These are out of scope for IPC tests derived from this file:

| Surface | Why not IPC |
| --- | --- |
| `PersistentObjectStore::{get, usage, verify_integrity, root, object_path, anchor_path}` | Local CAS / layout, not replication |
| `IntegrityReport` | Checksum scan of primary `*.obj` |
| ARKA1 / ARKIDX2 (`arkfs_core::codec`) | On-disk records, not peer frames `[S13]` |
| FUSE close / release / flush persist errors | `ArkSession`, not `ReplicationBackend`. Swallowing persist on close is a durability bug in the facade (`[S12]`); see maintainer.md, not this file |
| `ArkError::Quorum` → FUSE `EIO` | Facade mapping in `fuse_facade::err::to_errno`; not a cluster wire code `[S19]` |
| `simulation_harness::NetworkSimulator` and Elixir delay / `clock_rate` | Delay bookkeeping. They do not send or ack bytes. Inconsistencies (one-way vs “RTT/2”, Elixir vs Rust `clock_rate`) are harness bugs, not IPC postconditions `[S17]` |
| `ClusterManager` | SPEC names only; no crate `[S14]` `[S18]` |
| InternalChannel four APIs | Phase 1; [appendix](#9-phase-1-internalchannel-not-testable) |

---

## 3. Cross-cutting MUST

**Must be true**

1. The local durable write always counts as **one extra ack**, added by
   `require_quorum`, never by the backend:
   `total = remote_acks.saturating_add(1)`,
   `always_on = backend.always_on_count().saturating_add(1)`. `[S4]` `[S5]`
2. A **new** `put` and every `set_anchor` return success only after local
   `write_fsync` of staging, quorum satisfaction, and `publish` (`rename` +
   parent-directory `fsync`). `[S3]` `[S10]`
3. An **existing** `put` integrity-checks the primary. An intact name is
   not rewritten; a checksum mismatch is rewritten on the new-object path.
   Both still `replicate_object` and still require quorum (SPEC “blocks
   until safe”). Failure does not unpublish an intact existing name. `[S3]` `[S16]`
4. Phase 0 FUSE uses `open_isolated_store` and `QuorumPolicy::OwnerOnly`
   only. `[S6]` `[S7]`
5. Cluster **tests** may use `LocalQuorum`. That is a second backend, not a
   contradiction with NoPeers. `[S15]`

**Must be false**

6. A backend includes the local write in its returned ack count
   (double-counting with `require_quorum`). `[S4]`
7. Isolated FUSE talks to peer nodes, iterates a peer list, or performs
   peer I/O. `[S6]` `[S7]`
8. Isolated FUSE (or `NoPeers`) creates `DIR/replicas/`. `[S9]`
9. `QuorumPolicy::n(k)` with insufficient acks still publishes a **new**
   primary name, or silently lowers `k`. `[S11]`
10. Existing `put` ignores quorum or swallows `replicate_object` `Err`.
    `[S3]` `[S16]`
11. Local `sync_all` is skipped on the durable write path (`write_fsync` /
    `publish`). `[S12]` `[S1]`

---

## 4. Per-function assertions

### `ReplicationBackend`

```text
replicate_object(id, data) -> Result<u32, ArkError>   // remote acks
replicate_anchor(name, id_bytes) -> Result<u32, ArkError>
always_on_count() -> u32                              // reachable remotes, not local
```

**Must be true**

1. The trait exposes exactly these three methods. `[S4]`
2. `replicate_*` return values are **remote** acks only. `[S4]`
3. `always_on_count` is reachable remotes **excluding** the local primary.
   `[S4]` `[S5]`

**Must be false**

4. A conforming backend reports local publish as one of its acks. `[S4]`

**Ambiguous (classified from the two in-tree impls)**

5. Whether `replicate_*` returns `Err` versus `Ok(0)` on total remote
   failure is **not** a trait-level MUST. `NoPeers` always `Ok(0)`.
   `LocalQuorum` always `Ok(u32)` (failed replica writes are skipped, not
   `Err`). A **custom** backend **may** return `Err`. `[S4]` `[S8]` `[S15]`
6. If `replicate_*` returns `Err`, `put` / `set_anchor` MUST fail before
   `require_quorum` and MUST NOT publish a new name. Staging is discarded.
   That is a store-caller MUST (see [`put`](#put) / [`set_anchor`](#set_anchor)),
   not a trait MUST.

### `NoPeers`

**Must be true**

1. `replicate_object` → `Ok(0)`; arguments unused; no I/O. `[S8]`
2. `replicate_anchor` → `Ok(0)`; arguments unused; no I/O. `[S8]`
3. `always_on_count` → `0`. `[S8]`
4. No peer-list iteration. `[S8]` `[S9]`

**Must be false**

5. `NoPeers` creates `replicas/` or any replica path. `[S9]`
6. `NoPeers` performs network I/O or directory I/O. `[S8]`

### `open_isolated_store(base)`

**Must be true**

1. Opens `PersistentObjectStore` at `base/primary` with backend `NoPeers`.
   `[S1]` `[S8]`
2. `objects/` and `anchors/` are created under that primary only. `[S1]`

**Must be false**

3. Creates `base/replicas`. `[S9]`

### `LocalQuorum`

**Must be true**

1. `new(replica_dirs)` creates `objects/` and `anchors/` under each replica
   path. `[S15]`
2. `remotes_under(base, names)` is `new` of `names[1..]` at
   `base/replicas/<name>`. `[S1]`
3. `Clone` shares the lost-set (interior `Arc`). `[S15]`
4. `replicate_object` writes `{dir}/objects/{id.to_hex()}.obj` using
   `write_fsync` (sibling tmp, `write_all`, `sync_all`, rename, parent
   directory `sync_all`). `[S1]` `[S15]`
5. `replicate_anchor` writes `{dir}/anchors/{name}` the same way. `[S15]`
6. `acks` skips replica names present in `lost`. `[S15]`
7. The returned ack count is the number of replica writes for which
   `write.is_ok()` is true. `[S15]`
8. `always_on_count` equals `|replica_dirs \ lost|`. `[S15]`
9. `lose_node(id)` causes later `replicate_*` and `always_on_count` to
   ignore that id. `[S15]`
10. `restore_node(id)` undoes `lose_node`. `[S15]`

**Must be false**

11. `LocalQuorum::replicate_object` or `replicate_anchor` returns `Err`
    (this type always `Ok(u32)`; failures are missing acks). `[S15]`
12. A lost node’s directory is written by a later `replicate_*`. `[S15]`

**Ambiguous**

13. Replica-side `get` / re-hash is not part of this backend. Primary
    `get` re-hashes; replicas are write-only from IPC’s point of view.
    `[S1]` `[S15]`
14. Timeouts, message schemas, and a numeric peer error space do not
    exist for this type. `[S15]`

### `open_local_quorum_store(base, replica_names)`

**Must be true**

1. `replica_names` empty → `ArkError::InvalidArgument`. `[S1]`
2. `replica_names[0]` is the primary **label only**, not a remote. `[S1]`
3. Remotes come from `LocalQuorum::remotes_under`. `[S1]`
4. Primary store root is `base/primary`. `[S1]`
5. A **single** name (for example `["owner"]` or `["local"]`) yields
   `LocalQuorum` with **zero** remotes: `always_on_count() == 0`, and
   `replicas/` is not created because the remote list is empty. `[S1]`
6. That single-name case is **not** `NoPeers`: the backend type is
   `LocalQuorum`. FUSE MUST still use `open_isolated_store`. `[S6]` `[S7]`

**Must be false**

7. Treating `open_local_quorum_store(base, &["local"])` as the FUSE
   isolation contract. `[S6]` `[S7]`

### `QuorumPolicy`

```text
n(k) -> QuorumPolicy          // panics if k == 0
required_acks(always_on) -> u32
is_satisfied(total_acks, always_on) -> bool
```

`always_on` and `total_acks` here are **including local** (the values
`require_quorum` passes in). `[S5]`

**Must be true**

| Policy | `required_acks(always_on)` | Satisfied when |
| --- | --- | --- |
| `OwnerOnly` | `1` | `total_acks >= 1` |
| `n(k)` (`k >= 1`) | `k` (not capped at `always_on`) | `total_acks >= k` |
| `AllAlwaysOn` | `always_on` | `total_acks >= always_on` |

1. `is_satisfied` is `total_acks >= required_acks(always_on)`. `[S5]`
2. `OwnerOnly` is satisfied by the local write alone, even if
   `always_on > 1`. `[S5]`
3. `n(k)` with `k > 1` and `always_on == 1` (zero remotes) is **not**
   satisfied. `[S11]`
4. `n(k)` with some remotes present still fails when
   `1 + remote_acks < k` (partial replica success). `[S5]` `[S11]`
5. `AllAlwaysOn` after `lose_node` uses the reduced
   `always_on_count() + 1`. `[S5]` `[S15]`
6. `n(0)` is not a value: `QuorumPolicy::n(0)` panics; the `Quorum`
   variant holds `NonZeroU32`. `[S5]`

**Must be false**

7. `OwnerOnly` requiring remotes. `[S5]`
8. `AllAlwaysOn` requiring currently **lost** nodes. `[S5]` `[S15]`
9. `n(k)` lowering `k` to `always_on` when remotes are missing. `[S11]`
10. A zero copy-count that is always satisfied. `[S5]`

### `require_quorum(remote_acks, quorum, staging)`

**Must be true**

1. `total = remote_acks.saturating_add(1)`. `[S4]`
2. `always_on = self.backend.always_on_count().saturating_add(1)`. `[S4]`
3. If `quorum.is_satisfied(total, always_on)` → `Ok(())` and staging is
   left for `publish`. `[S4]` `[S5]`
4. Otherwise: if `staging` is `Some(p)`, delete `p` (errors from
   `remove_file` ignored); return `ArkError::Quorum { got: total,
   need: quorum.required_acks(always_on) }`. `[S4]` `[S11]`

**Must be false**

5. Publishing (rename to the final name) before this function returns
   `Ok`. `[S10]` `[S11]`
6. Leaving staging as the **final** object/anchor name on failure.
   `[S11]`
7. Silently reducing `need` below `required_acks`. `[S11]`

### `write_fsync` / `publish` (durable primitive used by IPC)

**Must be true**

1. `write_fsync(path, data)`: create parent dirs; write a sibling
   `*.tmp`; `write_all`; `sync_all`; `rename` tmp → `path`; `fsync` the
   parent directory. `[S1]` `[S10]` `[S12]`
2. `publish(staging, final)`: `rename` staging → final; `fsync` the
   parent directory. `[S1]` `[S10]`
3. Staging sibling of `abc.obj` is `abc.obj.tmp` (unique per final
   filename). `[S1]`

**Must be false**

4. Writing the final name in place without a tmp + `sync_all` + rename.
   `[S1]` `[S12]`
5. `publish` without parent-directory `fsync`. `[S10]` `[S12]`

### `put`

**Must be true (new object — `final_path` does not exist)**

1. Order: `ObjectId::from_bytes(data)` → `write_fsync(staging, data)` →
   `backend.replicate_object(&id, data)` → `require_quorum` → `publish`
   → `Ok(id)`. `[S1]` `[S3]` `[S10]`
2. If `replicate_object` returns `Err`, discard staging, do not
   `require_quorum`, do not `publish`. The **final** `{hex}.obj` MUST NOT
   exist; leftover `*.tmp` MUST NOT remain. `[S1]`
3. If `require_quorum` fails: no `{hex}.obj`; staging deleted; later
   `get(&id)` is `NotFound`. `[S11]`

**Must be true (existing object — `final_path` exists)**

4. `get(&id)` (integrity check). Intact → no rewrite (staging `None`).
   `Integrity` → staging `write_fsync` as for a new object. Other `get`
   errors return immediately. `[S16]`
5. Then call `replicate_object` and **do not ignore** its `Result`. `[S16]`
6. Then `require_quorum` (staging `None` when intact). `[S3]` `[S16]`
7. Return `Ok(id)` **without** rewriting an intact primary. `[S16]`
8. Replicate or quorum failure MUST NOT delete an already-published
   primary name (intact or still-corrupt). `[S16]`

**Must be false**

9. New-object success before `require_quorum` returns `Ok`. `[S10]`
10. Treating existing-object catch-up as a path that skips quorum. `[S16]`
11. `n(k)` with `k > 1` and zero remotes succeeding on a **new** object.
    `[S11]`

### `set_anchor`

**Must be true**

1. `validate_anchor_name(name)` first: non-empty `[A-Za-z0-9_]+`. On
   failure, `InvalidArgument` and **no** replica I/O. `[S1]`
2. Always: staging `write_fsync` of the 32-byte id → `replicate_anchor`
   → `require_quorum` → `publish`. `[S10]`
3. That sequence runs even when the anchor **already exists** (unlike
   intact existing-object `put`, which does not rewrite the primary). `[S1]` `[S16]`
4. If `replicate_anchor` returns `Err`, discard staging; the final
   anchor name is unchanged (overwrite does not publish). `[S1]`

**Must be false**

5. Skipping quorum on anchor overwrite. `[S10]` `[S16]`
6. Replicating or publishing an invalid name. `[S1]`

### FUSE wiring (isolation only)

**Must be true**

1. `ArkSession::mount_store` calls `open_isolated_store(data_dir)` then
   `TemporalCore::open(store, QuorumPolicy::OwnerOnly)`. `[S6]` `[S7]`

**Must be false**

2. FUSE using `LocalQuorum` or `open_local_quorum_store`. `[S6]` `[S7]`
3. FUSE creating `data_dir/replicas`. `[S9]`

---

## 5. Call-graph contract

### New `put`

```text
id = BLAKE3(data)
if primary objects/{id}.obj exists → existing-object path (below)
write_fsync(objects/{id}.obj.tmp, data)          // local durable
remote = backend.replicate_object(id, data)      // Err → discard staging, no publish
require_quorum(remote, policy, staging)          // fail → delete staging, Quorum{got,need}
publish(staging, objects/{id}.obj)               // rename + dir fsync
Ok(id)
```

### Existing `put`

```text
match get(id):
  Ok(_)                  staging = None          // intact; no rewrite
  Integrity              write_fsync(staging)    // rewrite like a new object
  other Err              return error
remote = backend.replicate_object(id, data)      // Err → discard staging; primary kept
require_quorum(remote, policy, staging)          // fail → Quorum{got,need}; primary kept
if staging: publish(staging, objects/{id}.obj)   // corrupt rewrite only
Ok(id)
```

### `set_anchor` (always)

```text
validate_anchor_name(name)?                      // else InvalidArgument, no I/O
write_fsync(anchors/{name}.tmp, id[32])
remote = backend.replicate_anchor(name, id[32])  // Err → discard staging, no publish
require_quorum(remote, policy, staging)
publish(staging, anchors/{name})
Ok(())
```

---

## 6. Error oracles (IPC)

| Condition | Error | `got` / `need` |
| --- | --- | --- |
| New `put` / `set_anchor`, `1 + remote_acks < required` | `ArkError::Quorum` | `got = 1 + remote_acks`, `need = required_acks(1 + always_on_count)` |
| Existing `put`, same ack shortfall | `ArkError::Quorum` | primary name **kept** |
| `NoPeers` + `n(2)` new put | `Quorum { got: 1, need: 2 }` | local only |
| `LocalQuorum` all nodes lost + `n(2)` | `Quorum { got: 1, need: 2 }` | same |
| Empty `open_local_quorum_store` names | `InvalidArgument` | — |
| Invalid anchor name | `InvalidArgument` | before replicate |
| Custom backend `replicate_*` `Err` | that error | no new publish; staging discarded |
| Existing `put`, primary checksum mismatch | rewrite path | quorum as for a new object; fail leaves corrupt name |

`got` **includes** the local write. `[S4]` `[S11]`

---

## 7. What a derived test suite MUST cover

A suite generated from this file is sufficient iff it includes at least:

| Target | Witness |
| --- | --- |
| `NoPeers` acks / always_on / no I/O | `replicate_*` → 0; no `replicas/` after `put` |
| `open_isolated_store` layout | root ends with `primary`; no `replicas/` |
| `OwnerOnly` / `AllAlwaysOn` on NoPeers | new put and `set_anchor` succeed |
| `n(2)` on NoPeers | fail closed; object name absent |
| `LocalQuorum` copy + fsync paths | replica `objects/` / `anchors/` exist with payload |
| `lose_node` / `restore_node` | lost skipped; restore counts again; `AllAlwaysOn` tracks live set **and replica dirs** |
| Partial replica success | `n(k)` fails when `1 + reachable < k`; live remote **has** bytes; primary unpublished |
| `open_local_quorum_store` arity | empty → error; one name → zero remotes; `skip(1)` under `replicas/` |
| New `put` order | no final name until quorum; staging gone on fail |
| Existing `put` | still requires quorum; catch-up after restore; corrupt primary rewritten; replicate `Err` keeps primary |
| `set_anchor` overwrite | still requires quorum |
| Invalid anchor name | `InvalidArgument`; no replica files |
| FUSE isolation | `mount_store` → isolated store + `OwnerOnly`; test
    `isolated_mount_has_no_replicas_dir` |

Witness tests (package `persistent_object_store` unless noted):

| Target | Test |
| --- | --- |
| `NoPeers` acks / always_on | `tests/ipc_conformance.rs::no_peers_replicate_returns_zero` |
| Isolated layout + OwnerOnly / AllAlwaysOn | `open_isolated_store_layout` |
| `n(2)` on NoPeers | `isolated_quorum_two_fails_closed` |
| `n(1)` isolated | `quorum_one_succeeds_isolated` |
| Replica copy + fsync | `local_quorum_copies_object_and_anchor` |
| lose / restore | `lose_node_skips_replica_restore_counts_again`; `double_lose_same_node`; `lose_unknown_and_restore_never_lost_are_nops` |
| AllAlwaysOn vs lost set | `all_always_on_tracks_lost_set` |
| Partial remotes below `n` | `quorum_fails_when_partial_remotes_below_n` |
| `open_local_quorum_store` arity | `open_local_quorum_store_arity` |
| Existing put + quorum | `existing_put_still_requires_quorum`; `existing_put_corrupt_primary_is_rewritten`; `existing_put_replicate_err_keeps_primary` |
| `set_anchor` overwrite | `set_anchor_overwrite_still_requires_quorum` |
| Invalid anchor name | `invalid_anchor_name_is_invalid_argument` |
| `replicate_*` `Err` | `replicate_err_does_not_publish` |
| `n(k)` not capped | `arkfs_core` `quorum_n_is_not_capped_at_always_on` |
| `n(0)` rejected | `arkfs_core` `quorum_n_rejects_zero` |
| FUSE isolation | `fuse_facade` `isolated_mount_has_no_replicas_dir` |

CAS / usage / staging-name / errno mapping stay in `persistent_object_store`
unit tests; they are not IPC witnesses.

---

## 8. Remaining latitudes (not MUST)

- Replica payload is not re-hashed by `LocalQuorum`.
- `remove_file(staging)` errors on the fail-closed path are ignored.
- `saturating_add` on ack counters (overflow is not a realistic replica
  count; specified as saturating because that is the code).
- Harness delay numbers and `clock_rate` (not a channel).
- How a future InternalChannel would implement `ReplicationBackend`.

---

## 9. Phase 1 InternalChannel (not testable)

SPEC.md names a custom QUIC + post-quantum + compressed channel: `[S2]`
`[S13]`

```text
connect, push_delta, request_historical, gossip_digest
QUIC → Handshake → Compression → Router
```

Facts, not testable MUST:

- No crate, module, or function by those names exists in the Rust
  workspace or Elixir packages. `[S14]`
- No argument types, return values, ordering, frames, error codes,
  timeouts, ciphersuites, key sizes, or compression codec. `[S2]`
- No mapping of those four calls onto `replicate_object` /
  `replicate_anchor` / `always_on_count`. `[S2]`
- No TLA+ spec in this tree. `[S13]`
- Quantum-secure high compression is a SPEC mandate, not an implemented
  check. `[S2]`

This file **MUST NOT** be used as a test plan for InternalChannel until
those are specified. Adding that protocol is a new library, not an edit
to these Phase 0 lists.

---

## Sources

- `[S1]` `libs/rust/persistent_object_store/src/lib.rs` (store, backends, `put` / `set_anchor` / `write_fsync` / `publish` / open helpers)
- `[S2]` `[S3]` `[S13]` `[S18]` [SPEC.md](../../SPEC.md) (InternalChannel names, safe-write diagram, Phase 1, ClusterManager names)
- `[S4]` `ReplicationBackend` + `require_quorum` in `persistent_object_store`
- `[S5]` `[S11]` `libs/rust/arkfs_core/src/quorum.rs`
- `[S6]` `[S7]` `[S12]` [maintainer.md](../maintainer.md) (isolation, FUSE `OwnerOnly`, durability)
- `[S8]` `NoPeers` impl
- `[S9]` `libs/rust/fuse_facade/src/session_tests.rs` (`isolated_mount_has_no_replicas_dir`); `NoPeers` never mkdirs
- `[S10]` [phase0.md](../phase0.md) (safe-write: fsync, quorum, publish)
- `[S14]` [Cargo.toml](../../Cargo.toml) (no InternalChannel member)
- `[S15]` `LocalQuorum` (`new`, `remotes_under`, `acks`, `lose_node`, `restore_node`)
- `[S16]` `PersistentObjectStore::put` existing-object branch
- `[S17]` `libs/rust/simulation_harness/src/lib.rs`; `libs/elixir/simulation_harness/lib/simulation_harness.ex` (not IPC)
- `[S19]` `libs/rust/fuse_facade/src/err.rs` (`Quorum` → `EIO`)
- FUSE open path: `libs/rust/fuse_facade/src/session.rs` (`mount_store`)
