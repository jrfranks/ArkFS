# FUSE conformance and automated reasoning

Onboarding: [maintainer.md](maintainer.md). This page is the ArkFS FUSE
**reachability table** and which tools GitHub vs local pushes run. It is not
the Linux/libfuse ABI inventory; that (not a test oracle) is
[conformance/fuse-abi.md](conformance/fuse-abi.md). IPC oracle:
[conformance/ipc.md](conformance/ipc.md).

Package tests (`make ci`, GitHub) run **without** `/dev/fuse` and **without**
formal proofs. The FUSE kernel adapter is a thin translator; POSIX behavior is
checked on `ArkSession`, the same type `FuseFs` calls.

Local pushes (`make prepush`, `.githooks/pre-push`) additionally require
clippy, Kani, Miri, llvm-cov, and live FUSE. Commits are not gated.

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
| rename | `fuse_rename`, `fuse_rename_directory_moves_children` |
| link | `fuse_link` |
| mknod (regular / fifo / device) | `fuse_mknod` |
| copy_file_range | `fuse_copy_file_range` |
| getlk / setlk | `fuse_getlk_setlk` |
| poll | `fuse_poll` |
| bmap | `fuse_bmap` |
| readdirplus | `fuse_readdirplus` |
| fallocate / lseek | `fuse_fallocate_lseek` |
| symlink / readlink | `fuse_symlink_readlink` |
| readdir / opendir / releasedir | `fuse_readdir` |
| setxattr / getxattr / listxattr / removexattr | `fuse_xattr_set_get_list_remove_sized` |
| mount (as-of, remount) | `fuse_never_delete_as_of_and_remount` |
| live kernel drive | `fuse_drive::fuse_all_ops_then_disk` — real mount, every op, CAS inspect after each |
| live `--as-of` remount | `fuse_drive::fuse_live_as_of_remount_reads_tombstoned_bytes` — unmount-not-needed: second mount of the same data dir, read unlinked bytes, CAS still has the object |

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
| cargo / mix test | package tests, in-process FUSE op reachability, CLI | `make ci` (GitHub) |
| clippy `-D warnings` | lints | `make prepush` (local only) |
| [Kani](https://github.com/model-checking/kani) | `write_then_read_returns_payload`, `negative_offset_reads_from_zero`, `range_iff_too_small` | `make prepush` |
| Miri | UB on PathKey parse (`path_parse`) | `make prepush` |
| cargo-llvm-cov | line coverage on `fuse_facade` (≥ 80%, not a substitute for `fuse.rs` review) | `make prepush` |

GitHub never runs Kani, Miri, or llvm-cov. Local `make prepush` **fails** if those tools are missing (not skipped).

Grok is used in-tree as the author of the reachability table and the POSIX
cases (no separate Grok verifier binary). Kani is the free model checker for
the pure cores.
