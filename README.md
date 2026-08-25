# ArkFS

Sovereign continuous temporal distributed file system. Data follows the owner (Earth → Moon → Mars). Never-delete with arbitrary timestamp access. Private clusters. Custom modular code (Elixir orchestration + Rust performance paths).

See [SPEC.md](SPEC.md) for the product specification and the implementation plan phases.

## Organization

**Make** is the monorepo organizer. Each deliverable is an **independent library** or an **app that includes libraries**. There is no Mix umbrella.

```
libs/rust/*     Rust libraries (Cargo workspace convenience under Make)
libs/elixir/*   Elixir libraries (each has its own mix.exs)
apps/*          Runnable apps that depend on libraries
```

## Quick start

```bash
make help
make test      # cargo test --workspace + per-package mix test
make build
make ci
```

## Phase 0 libraries

| Library | Role |
|---------|------|
| `arkfs_core` | Shared types + **FileAttributes** superset (FUSE/NFS/SMB3/WebDAV/macOS) |
| `simulation_harness` | Interplanetary sim: clock, delay, chaos |
| `persistent_object_store` | Safe-write object store (fsync + quorum) |
| `temporal_core` | Cactus-stack temporal engine |

| App | Role |
|-----|------|
| `sim_runner` | Runs Elixir harness scenarios (clock / delay / chaos). Store and temporal tests live in `cargo test`. |

## Attribute model

Canonical metadata is a superset of protocol needs. Facades map via `arkfs_core::attr_map`. Details: [docs/attributes.md](docs/attributes.md).

## License

UNLICENSED — all original work.
