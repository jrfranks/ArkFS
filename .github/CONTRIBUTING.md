# Contributing

If you are new to this repository, read [docs/maintainer.md](../docs/maintainer.md) first. It explains the layering, disk layout, never-delete semantics, and the GitHub vs local test split.

Make is the monorepo organizer. Cargo and Mix are per-package tools.

```bash
make help
make test
make fmt
make ci            # GitHub: fmt + package tests only
make install-hooks # once: pre-commit runs the full local bar
make precommit     # all tests + clippy + Kani + Miri + llvm-cov
```

Libraries live under `libs/`; runnable apps under `apps/`. There is no Mix umbrella.

- Canonical file metadata is `arkfs_core::FileAttributes`. Protocol facades (`attr_map`) are projections only.
- Durable bytes go through `persistent_object_store` (`put` / `get`). Temporal history is `temporal_core`.
- Store and temporal guarantees are asserted in `cargo test`. The Elixir harness covers clock, delay, and chaos bookkeeping only.

GitHub Actions runs **`make ci` only**: rustfmt, mix format, `cargo test --workspace`, Elixir `mix test`. No clippy-as-error, Kani, Miri, or coverage.

Commits on a machine with hooks installed (`make install-hooks`) run **`make precommit`**: the GitHub set plus clippy `-D warnings` and required Kani, Miri, and llvm-cov. Emergency bypass: `git commit --no-verify` or `ARKFS_SKIP_PRECOMMIT=1`.

Code owner is [@jrfranks](https://github.com/jrfranks). The project is MIT licensed (see [LICENSE](../LICENSE)).
