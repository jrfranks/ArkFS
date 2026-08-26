# ArkFS maintainer guide

This is the onboarding document for engineers who have not worked on a
filesystem, a content-addressed store, or a FUSE adapter before. Read this
before changing code. Product intent lives in [SPEC.md](../SPEC.md); attribute
rules live in [attributes.md](attributes.md); FUSE user docs live in
[fuse.md](fuse.md).

## What ArkFS is

ArkFS is a **never-delete temporal filesystem**. Users see a POSIX tree today.
Every write, mkdir, unlink, and setattr appends a new **version** rather than
overwriting history. Later you can mount `--as-of LOGICAL` and see the tree as
it was at that logical time. Bytes that were ever stored stay on disk until a
future GC policy exists (none in Phase 0).

Phase 0 is a **single-node** implementation: one process, one data directory,
Linux FUSE. Multi-node replication exists as a library interface so cluster
work can plug in later. The FUSE app must behave as if **no other nodes are
connected**.

## Mental model (five minutes)

```
  user process  (ls, cat, echo > file)
        |
        v
  Linux FUSE kernel  (/dev/fuse)
        |
        v
  fuse_facade::FuseFs     thin translator: inodes, errno, open-file cache
        |
        v
  fuse_facade::ArkSession in-process node: fh table, whole-object commit
        |
        v
  temporal_core           POSIX tree + version history (the cactus)
        |
        v
  persistent_object_store content-addressed bytes + named anchors
        |
        v
  data/primary/{objects,anchors}   files on local disk
```

Rules of thumb:

| If you need to change… | Go here |
|------------------------|---------|
| POSIX semantics (mkdir, unlink, readdir, as-of) | `libs/rust/temporal_core` |
| Durable bytes, fsync, quorum | `libs/rust/persistent_object_store` |
| Shared types, paths, errors, attributes | `libs/rust/arkfs_core` |
| Kernel FUSE glue, errno, open cache | `libs/rust/fuse_facade` |
| CLI `arkfs mount` | `apps/arkfs` |
| Clock / delay / chaos bookkeeping | `simulation_harness` (Rust + Elixir) |
| Build / test / CI | `Makefile`, `make/*.mk`, `.github/workflows/ci.yml` |

Do **not** put filesystem logic in `FuseFs`. That type maps FUSE requests onto
`ArkSession`. Do **not** put POSIX tree logic in the object store. The store
only knows bytes and names.

## Glossary

These words show up in comments and types. Use them consistently.

| Term | Meaning |
|------|---------|
| **CAS** | Content-addressed store. Object filename is `blake3(bytes)` hex + `.obj`. Changing one bit changes the name. |
| **ObjectId** | 32-byte BLAKE3 of a payload. Printed as 64 hex chars. |
| **Anchor** | A named 32-byte pointer in `anchors/` (today: `temporal_index`). Not content-addressed; it moves as the index grows. |
| **Safe-write** | `put` / `set_anchor` return only after local `fsync`, quorum acks, then `rename` of the published name. |
| **Quorum** | How many durable copies count as “safe”. Local write always counts as 1. |
| **OwnerOnly** | Quorum policy used by single-node FUSE: one local ack is enough. |
| **ReplicationBackend** | Trait the store uses to copy bytes to peers. `LocalQuorum` writes extra directories. Isolated / no-peers should return 0 acks and create no replica dirs. |
| **Cactus stack** | Per-path list of versions. Each version points at the previous index (`parent`). Branches are extra versions, not overwrites. |
| **Tombstone** | A version that means “this name is gone in this view.” Bytes stay. Live lookup returns not-found. |
| **View** | `Live` = latest version of each path. `AsOf(ts)` = last version with `at <= ts`. |
| **Logical time** | Monotonic counter on `Timestamp`. `--as-of N` is this number, not wall-clock. |
| **FileAttributes** | Canonical metadata for one version. FUSE/NFS/SMB/WebDAV/macOS are *projections*. |
| **attr_map** | Read-modify-write helpers so a FUSE setattr cannot wipe SMB DOS flags. |
| **Inode / file_id** | 64-bit identity in attributes. FUSE root is always inode `1` (`FUSE_ROOT_ID`). |
| **fh** | File handle integer the kernel uses for open files. Distinct from inode. |
| **Facade** | Protocol adapter. Must not be source of truth. |
| **Never-delete** | Unlink does not `unlink(2)` objects. It appends a tombstone. |
| **Make organizer** | Root `Makefile` walks packages. There is **no** Mix umbrella. |

## Repository layout

```
Makefile                 # organizer: build, test, ci, precommit
make/rust.mk             # cargo workspace wrappers
make/elixir.mk           # per-package mix (no umbrella)
make/ci.mk               # GitHub bar: fmt + tests
make/verify.mk           # local bar: clippy + Kani + Miri + llvm-cov + live FUSE
.githooks/pre-commit     # runs `make precommit` unless ARKFS_SKIP_PRECOMMIT=1
Cargo.toml               # Rust workspace members + shared deps
libs/rust/arkfs_core     # types, codec, paths, quorum, attr_map
libs/rust/persistent_object_store
libs/rust/temporal_core
libs/rust/fuse_facade
libs/rust/simulation_harness
libs/elixir/simulation_harness
apps/arkfs               # FUSE CLI
apps/sim_runner          # Elixir harness scenarios
docs/                    # human docs (this file, fuse, attributes, phase0)
```

`target/` and Elixir `_build/` are generated. Do not commit them.

## Data on disk

`--data DIR` is the store root. After a FUSE mount you should see:

```
DIR/
  primary/
    objects/
      <64-hex>.obj          # file bytes, encoded attrs, or encoded index
    anchors/
      temporal_index        # 32 raw bytes: ObjectId of the latest index object
```

There should **not** be `DIR/replicas/` on a single-node mount. If a change
creates replica directories for FUSE, that is a bug: the comms path is no
longer isolated.

Object files are immutable. Updating a file writes a *new* object and then
moves the `temporal_index` anchor. Old objects remain (never-delete).

Staging uses a sibling tempfile (`abc.obj.tmp`) then `rename` + directory
`fsync`. A leftover `.tmp` after a crash is unpublished; tests treat stray
`.tmp` as a failure.

## Request path for `echo hi > /mnt/ark/f`

1. Kernel `create` / `open` / `write` / `flush` / `release` over FUSE.
2. `FuseFs` parses the UTF-8 name, calls `ArkSession`.
3. `create_file` in TemporalCore: `prepare_create` (parent is a dir, name free),
   `commit_branch` with empty content, new `file_id`.
4. `write` mutates the **open-file cache** (`Vec<u8>` on the fh). Nothing hits
   the store yet.
5. `fsync` / `flush` / `release` call `replace_content`, which `put`s new bytes
   and a new attr object, then persists a new index.
6. `PersistentObjectStore::put` hashes bytes → stages → replicate → quorum →
   publish.

If you skip step 5 (close without fsync and `release` swallows errors), user
data can vanish. Treat that as a durability bug.

## Never-delete in practice

```
create /a.txt          versions = [v0 live]
write /a.txt           versions = [v0, v1 live]
unlink /a.txt          versions = [v0, v1, v2 tombstone]
lookup Live /a.txt     NotFound
lookup AsOf(v1)        content of v1
readdir Live /         does not list a.txt
```

`readdir` is a prefix scan of the path map (`PathKey::immediate_child`), not a
directory inode with child pointers. Adding a child does not rewrite the
parent directory object.

## Quorum and isolation

`QuorumPolicy` lives in `arkfs_core`. The store adds **1** for the local write:

```
total_acks  = remote_acks + 1
always_on   = backend.always_on_count() + 1
```

| Policy | Need |
|--------|------|
| `OwnerOnly` | 1 (local only) |
| `Quorum(n)` | `n` including local |
| `AllAlwaysOn` | every reachable copy including local |

Single-node FUSE uses `OwnerOnly`. The replication backend for that path must
act as **no peers**: `replicate_*` returns 0, `always_on_count` is 0, no
replica directories, no network. `LocalQuorum` with `replica_names = ["local"]`
happens to have an empty remote list via `skip(1)` — that is implicit, not
explicit isolation. Prefer an explicit no-peers backend when adding or
reviewing that path.

`Quorum(n)` with `n > 1` and zero remotes must **fail closed** (no publish).

## FUSE specifics

- Linux only (`fuser` with `default-features = false`, `abi-7-31` for
  `fallocate` / `lseek`).
- Names must be UTF-8; otherwise `EINVAL`.
- Root inode is `FUSE_ROOT_ID` (1), even if some other `file_id` is stored.
- TTL is 1s (`session::TTL`). The kernel may cache attrs that long.
- `IMPLEMENTED_FUSE_OPS` is the reachability contract. Adding a
  `Filesystem` method requires a table entry and a `fuse_*` test.
- Production `mount()` currently passes `AutoUnmount` + `DefaultPermissions`.
  `AutoUnmount` without `AllowOther` makes fusermount request `allow_other`,
  which stock `/etc/fuse.conf` often disables. Tests use `spawn()` without
  those flags. Prefer the test flags for unprivileged mounts.
- Do **not** implement `getlk`/`setlk`/`poll` as no-ops unless you understand
  the kernel fallback: ENOSYS from userspace restores in-kernel POSIX locks
  and “always ready” poll. Returning success with `F_UNLCK` or `poll(0)`
  *opts in* to userspace and then does nothing useful.

Live tests: `libs/rust/fuse_facade/tests/fuse_drive.rs`. They skip when `CI`
is set and `/dev/fuse` is missing. Local pre-commit sets
`ARKFS_REQUIRE_FUSE=1` and **fails** if FUSE is absent.

## How to build and test

```bash
make help
make test          # cargo test --workspace + mix test per package
make ci            # GitHub: rustfmt + mix format + those tests
make install-hooks # once per clone: git hooksPath -> .githooks
make precommit     # local: ci + clippy -D warnings + Kani + Miri + llvm-cov + live FUSE
```

GitHub Actions (`.github/workflows/ci.yml`) runs **`make ci` only**. It must
not require `/dev/fuse`, Kani, Miri, or llvm-cov.

Emergency commit bypass: `git commit --no-verify` or `ARKFS_SKIP_PRECOMMIT=1`.
Do not use this to skip a failing invariant.

### Formal tools (local only)

| Tool | What it checks | Target |
|------|----------------|--------|
| rustfmt | formatting | `cargo fmt --all -- --check` |
| clippy `-D warnings` | lints as errors | workspace |
| Kani | bounded proofs on `io_buf`, `xattr`, `PathKey`, errno table | `fuse_facade`, `arkfs_core` |
| Miri | UB on PathKey parse | `arkfs_core` `path_parse` |
| llvm-cov | line coverage on `fuse_facade` (≥ 80%) | live FUSE included when required |

`#[cfg(kani)]` modules are compiled only under Kani. Workspace
`unexpected_cfgs` is configured with `check-cfg=['cfg(kani)']`.

## How to add a FUSE operation

1. Implement POSIX behavior on `ArkSession` / `TemporalCore` with a unit test
   that does **not** need `/dev/fuse`.
2. Add the `Filesystem` method in `fuse.rs`. Map errors with `err::to_errno`.
3. Add the op name to `IMPLEMENTED_FUSE_OPS`.
4. Add `fuse_<op>` in `tests/conformance.rs` (the reachability parser greps
   for this).
5. Drive it from the live scaffold (`tests/fuse_drive.rs`) and assert on-disk
   structure with `inspect()`.
6. If the op needs a newer FUSE ABI, bump `fuser` features in workspace
   `Cargo.toml` and document why.

## How to add a FileAttributes field

1. Add the field to `FileAttributes` with a default.
2. Encode and decode it in `arkfs_core::codec` (lossless; tests round-trip a
   rich record). Bump `ATTR_MAGIC` (`ARKA1`) only if the layout is
   incompatible — prefer appending with an option tag if you can stay on
   `ARKA1`.
3. Project and merge in the relevant `attr_map` module. Unset `Option` on
   setattr must **not** clear the field.
4. Update [attributes.md](attributes.md) field matrix.
5. Temporal index does **not** store full attrs; it stores `attrs_id` pointing
   at a CAS object. Existing indexes remain valid.

## How to add a replication backend

Implement `ReplicationBackend`:

```text
replicate_object(id, bytes) -> remote acks
replicate_anchor(name, id_bytes) -> remote acks
always_on_count() -> reachable remotes (not including local)
```

Then pass `Box::new(your_backend)` to `PersistentObjectStore::open`.

- Cluster tests: `LocalQuorum` + `open_local_quorum_store`.
- Single-node FUSE: no peers. `replicate_*` → `Ok(0)`, `always_on_count` → 0,
  never mkdir `replicas/`.

Do not call `skip(1)` and hope an empty vec means isolated. Name the type so
the next reader can see the intent.

## Locks and concurrency

`TemporalCore` has three mutexes:

| Lock | Protects |
|------|----------|
| `commit` | One writer at a time. Take this before mutating durable index. |
| `index` | In-memory cache of the durable index. Do not do I/O while holding it. |
| `clock` | Logical/wall timestamp. |

Pattern: clone the index snapshot, drop the index lock, `put` objects, then
`persist_index`. Holding `index` across `store.put` will deadlock or stall
lookups.

`ArkSession.files` is a mutex around the fh map. `fsync` clones dirty bytes,
drops the lock, writes, then re-locks to clear `dirty`. That avoids I/O under
the fh lock.

`.lock().unwrap()` means “poisoned mutex = prior panic”. That is intentional
fail-fast, not a forgotten error path.

## Common maintenance traps

These are easy to get wrong. Check them in review.

1. **Inode 0.** `commit_branch` allocates `file_id` only when the *path is
   new*. A tombstone still occupies the path key, so unlink+create can store
   `file_id = 0`. FUSE treats nodeid 0 as “negative lookup” (ENOENT). Always
   allocate a new id when the last version is a tombstone, and never publish
   inode 0 for a non-root.
2. **Directory rename.** `rename` copies one path and tombstones the source.
   Child paths are not rewritten. `mkdir a/x; rename a b` currently leaves
   `/a/x` live. Either implement subtree retarget or reject directory rename.
3. **Open-file cache is per-fh.** Two fhs do not share a buffer. `O_TRUNC`
   dirties one fh only. `OpenFile.path` is a snapshot: rename/unlink then
   fsync writes the old path.
4. **`release` ignores fsync errors.** `let _ = self.fsync(fh)` then drops
   the buffer. Close can lose dirty data.
5. **Conflict-before-write.** `commit_branch` rejects `at <= last.at` *before*
   `put`. Do not persist then roll back.
6. **Partial setattr.** Always `merge_from_*`, never replace the whole
   `FileAttributes` from a protocol struct.
7. **GitHub vs local.** A test that needs `/dev/fuse` must skip when `CI` is
   set. A test that is the local bar must fail when `ARKFS_REQUIRE_FUSE=1`
   and FUSE is missing.
8. **Elixir harness is not the store.** `SimulationHarness` records clock,
   delay, and chaos *intent*. Integrity of CAS and temporal lookup is proven
   in `cargo test` on the Rust crates.

## Where tests live

| Package | What they prove |
|---------|-----------------|
| `arkfs_core` | Path parse, ObjectId, quorum math, lossless attr codec, attr_map RMW |
| `persistent_object_store` | put/get, checksum, quorum fail-closed, OwnerOnly, anchors |
| `temporal_core` | commit, as-of, tombstone, readdir, reopen, conflict |
| `fuse_facade` unit | session mkdir/write/as-of, errno table, io_buf, xattr sized |
| `fuse_facade` `conformance.rs` | every named FUSE op without `/dev/fuse` |
| `fuse_facade` `fuse_drive.rs` | live mount + `inspect()` after each op |
| `arkfs` `cli_usability.rs` | argv parse + binary exit codes |
| Elixir `simulation_harness` | clock ticks, delay override, chaos log |
| `sim_runner` | named scenarios + oracles |

## Reading order for a first week

1. This file.
2. [phase0.md](phase0.md) — what Phase 0 delivered.
3. `arkfs_core/src/id.rs` (`PathKey`, `ObjectId`) and `error.rs`.
4. `persistent_object_store/src/lib.rs` (`put` / `require_quorum`).
5. `temporal_core/src/lib.rs` (`commit_branch`, `tombstone`, `readdir`).
6. `fuse_facade/src/session.rs` then `fuse.rs`.
7. `apps/arkfs/src/cli.rs` + `main.rs`.
8. `tests/conformance.rs` and `tests/fuse_drive.rs`.

When in doubt: **bytes are immortal, names are versioned, FUSE is a facade.**
