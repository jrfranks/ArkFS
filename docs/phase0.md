# Phase 0 — Foundation

Onboarding for the code itself is in [maintainer.md](maintainer.md). This page is the Phase 0 package list and durability contract.

## Packages

| Package | Kind | Path |
|---------|------|------|
| `arkfs_core` | Rust library | `libs/rust/arkfs_core` |
| `simulation_harness` | Rust library | `libs/rust/simulation_harness` |
| `persistent_object_store` | Rust library | `libs/rust/persistent_object_store` |
| `temporal_core` | Rust library | `libs/rust/temporal_core` |
| `fuse_facade` | Rust library | `libs/rust/fuse_facade` |
| `simulation_harness` | Elixir library | `libs/elixir/simulation_harness` |
| `sim_runner` | Elixir app | `apps/sim_runner` |
| `arkfs` | Rust app | `apps/arkfs` |

## Safe-write contract

`PersistentObjectStore` is a content-addressed byte store. `put` returns success only after:

1. Local durable staging write (`fsync` of the object tempfile and parent directory), skipped when an intact primary already exists; a checksum-mismatch primary is rewritten
2. Quorum acknowledgments from `ReplicationBackend` (new and existing objects; Phase 0 FUSE uses `NoPeers` + `OwnerOnly`)
3. Atomic publish of the primary object name (`rename` + directory `fsync`), new objects and corrupt rewrites only

IPC contract: [conformance/ipc.md](conformance/ipc.md).

Identity is `ObjectId::from_bytes(payload)` (BLAKE3). There is no sidecar metadata file.
`get` rehashes and fails closed on mismatch. `set_anchor` / `get_anchor` persist named
root pointers (used by TemporalCore for `temporal_index`).

## Temporal index

`TemporalCore` keeps an in-memory cache of a durable index object pointed at by the
`temporal_index` anchor. Restarts reload path history from the store.

## Build

```bash
make setup     # clone/checkout: install local tools + git hooks
make test      # all packages (also sets git hooksPath)
make test-rust
make test-elixir
make ci
make test         # cargo test (debug) + mix test
make sim          # Elixir scenario catalog (escript)
make release      # target/release/arkfs only
make install      # $(PREFIX)/bin/arkfs (no Mix)
```

Make is the monorepo organizer. Mix is used only inside each Elixir package directory.
Rust `cargo test` covers the store and the temporal index. `make sim` / `mix test`
exercise the Elixir harness (clock, delay, chaos log, history oracle) only.
Those are not `make release`.
