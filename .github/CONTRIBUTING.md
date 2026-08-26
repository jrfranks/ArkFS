# Contributing

If you are new to this repository, read [docs/maintainer.md](../docs/maintainer.md) first. It explains the layering, disk layout, never-delete semantics, and the GitHub vs local test split.

Make is the monorepo organizer. Cargo and Mix are per-package tools.

```bash
make setup         # clone/checkout: install rustfmt/clippy/Kani/Miri/llvm-cov + git hooks
make help
make test          # package tests (does not install Kani)
make fmt
make ci            # GitHub: fmt + package tests only (no proof tools)
make prepush       # all tests + clippy + Kani + Miri + llvm-cov (git push hook)
```

Libraries live under `libs/`; runnable apps under `apps/`. There is no Mix umbrella.

- Canonical file metadata is `arkfs_core::FileAttributes`. Protocol facades (`attr_map`) are projections only.
- Durable bytes go through `persistent_object_store` (`put` / `get`). Temporal history is `temporal_core`.
- Store and temporal guarantees are asserted in `cargo test`. The Elixir harness covers clock, delay, and chaos bookkeeping only.

GitHub Actions runs **`make ci` only**: rustfmt, mix format, `cargo test --workspace`, Elixir `mix test`. No clippy-as-error, Kani, Miri, or coverage.

The first `make setup`, `make test`, or `make build` after clone sets `core.hooksPath` to `.githooks`. After that, `git checkout` / `git merge` / `git pull` run `.githooks/post-checkout` and `post-merge`, which call `scripts/setup-tools.sh`. There is no pre-commit hook. **`git commit` does not run tests.** **`git push` runs `make prepush`.** Emergency bypass: `git push --no-verify`, `ARKFS_SKIP_PREPUSH=1`, or `ARKFS_SKIP_SETUP=1`.

Code owner is [@jrfranks](https://github.com/jrfranks). The project is MIT licensed (see [LICENSE](../LICENSE)).
