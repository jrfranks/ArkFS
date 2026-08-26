# Contributing

Make is the monorepo organizer. Cargo and Mix are per-package tools.

```bash
make help
make test
make fmt
make ci
```

Libraries live under `libs/`; runnable apps under `apps/`. There is no Mix umbrella.

- Canonical file metadata is `arkfs_core::FileAttributes`. Protocol facades (`attr_map`) are projections only.
- Durable bytes go through `persistent_object_store` (`put` / `get`). Temporal history is `temporal_core`.
- Store and temporal guarantees are asserted in `cargo test`. The Elixir harness covers clock, delay, and chaos bookkeeping only.

Pull requests should stay green on `.github/workflows/ci.yml` (`make ci`: rustfmt, mix format, clippy `-D warnings`, all package tests). Code owner is [@jrfranks](https://github.com/jrfranks). The project is MIT licensed (see [LICENSE](../LICENSE)).
