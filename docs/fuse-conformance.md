# FUSE conformance and automated reasoning

Onboarding: [maintainer.md](maintainer.md). This page is the reachability table
and which tools GitHub vs local commits run.

Package tests (`make ci`, GitHub) run **without** `/dev/fuse` and **without**
formal proofs. The FUSE kernel adapter is a thin translator; POSIX behavior is
checked on `ArkSession`, the same type `FuseFs` calls.

Local commits (`make precommit`, `.githooks/pre-commit`) additionally require
clippy, Kani, Miri, and llvm-cov.

## Reachability

`IMPLEMENTED_FUSE_OPS` is the contract. `fuse_impl_source_matches_reachability_table`
parses `src/fuse.rs` and fails if a `Filesystem` method is added without updating
the table or a named `fuse_*` conformance test.

| FUSE op | Witness |
|---------|---------|
| lookup | `fuse_lookup` |
| getattr | `fuse_getattr` |
| setattr | `fuse_setattr_mode_uid_gid_size_times`, `fuse_setattr_size_via_open_fh` |
| mkdir | `fuse_mkdir` |
| create / open / read / write / flush / fsync / release | `fuse_create_open_read_write_flush_fsync_release` |
| unlink / rmdir | `fuse_unlink_rmdir` |
| rename | `fuse_rename` |
| symlink / readlink | `fuse_symlink_readlink` |
| readdir / opendir / releasedir | `fuse_readdir` |
| setxattr / getxattr / listxattr / removexattr | `fuse_xattr_set_get_list_remove_sized` |
| mount (as-of, remount) | `fuse_never_delete_as_of_and_remount` |
| live kernel drive | `fuse_drive::fuse_all_ops_then_disk` — real mount, every op, CAS inspect after each |

Errno mapping is exhaustive: `to_errno` is a total `match` on `ArkError`;
`errno_table_covers_every_variant` checks the table.

## Command options (`arkfs`)

`arkfs::cli::parse` is pure. Tests cover `--help`/`-h`/`help`, `mount --data`,
`--as-of`, missing operands, unknown flags/commands, extra args, `umount`/`unmount`.
The binary is exec'd for usability (exit codes, stderr contains `usage`).

## Proveability tools (free)

| Tool | What | How |
|------|------|-----|
| rustc exhaustiveness | `ArkError` → errno, `FileType` → FUSE kind | compile-time `match` |
| rustfmt | format | `make ci` (GitHub) |
| cargo / mix test | package + FUSE conformance + CLI | `make ci` (GitHub) |
| clippy `-D warnings` | lints | `make precommit` (local only) |
| [Kani](https://github.com/model-checking/kani) | `read_slice`/`apply_write`, xattr sizing, PathKey, errno table | `make precommit` |
| Miri | UB on PathKey parse | `make precommit` |
| cargo-llvm-cov | session/errno/io ≥ 90% lines (not `fuse.rs`) | `make precommit` |

GitHub never runs Kani, Miri, or llvm-cov. Local `make precommit` **fails** if those tools are missing (not skipped).

Grok is used in-tree as the author of the reachability table and the POSIX
cases (no separate Grok verifier binary). Kani is the free model checker for
the pure cores.
