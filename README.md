# ArkFS

[![CI](https://github.com/jrfranks/ArkFS/actions/workflows/ci.yml/badge.svg)](https://github.com/jrfranks/ArkFS/actions/workflows/ci.yml)

ArkFS keeps every committed version of a path. `unlink` and `rmdir` append a tombstone, so the live name disappears and `arkfs mount --data DIR --as-of N` can still read the tree at logical commit N. That mount is read-only. Objects stay in the content-addressed store; this tree has no garbage collector.

The mount is one Linux node (`fuse3`, `/dev/fuse`). Rust owns the object store and the temporal index. `arkfs mount` opens `open_isolated_store`: local `fsync`, directory `fsync` after `rename`, `QuorumPolicy::OwnerOnly`, and no `replicas/` directory. Elixir runs the scenario catalog (logical clock, a delay field, a log of named bit-flips and lost nodes). It does not open the store. A Rust store test drains that log and flips one bit in a published object; `get` then fails the BLAKE3 check.

GitHub Actions runs `make ci`: format, `cargo test`, and `mix test`, with no `/dev/fuse`. The live kernel mount is `tests/fuse_drive.rs`, skipped when `CI=1`. Kani and Miri run only on a local `make prepush`, and only on the buffer, xattr-size, and path-parse helpers.

This is Phase 0. [SPEC.md](SPEC.md) is the roadmap for what is not in this tree. If you are going to change code, start with [docs/maintainer.md](docs/maintainer.md). IPC behavior of the store is [docs/conformance/ipc.md](docs/conformance/ipc.md).

## Layout

Make is the organizer. Each crate or Mix project stands on its own. There is no Mix umbrella.

```
libs/rust/*     Rust libraries
libs/elixir/*   Elixir libraries (each has its own mix.exs)
apps/*          runnable apps that depend on those libraries
```

## Quick start

```bash
git clone https://github.com/jrfranks/ArkFS.git
cd ArkFS
make setup     # rustfmt, clippy, Kani, Miri, llvm-cov; enables git hooks
make test
```

`make setup` is safe to re-run. `make test` and `make build` also arm the git hooks, so you do not need a separate setup step. After that, `git checkout` / `git pull` re-run the installer. `make test` does not install Kani. GitHub sets `CI` and only runs `make ci`. Commits are not gated. `git push` runs `make prepush`. Details: [`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md).

## Mount it (Linux)

Phase 0 `arkfs mount` is Linux-only (`fuse3`, `/dev/fuse`, `fusermount3`). You need the FUSE client plus headers used to build `fuser`.

More mount notes: [docs/fuse.md](docs/fuse.md).

```bash
# Debian / Ubuntu
sudo apt-get update
sudo apt-get install -y fuse3 libfuse3-dev

# Fedora / RHEL
sudo dnf install -y fuse3 fuse3-devel

# Arch
sudo pacman -S fuse3

# openSUSE
sudo zypper install -y fuse3 fuse3-devel
```

Check that the device and helper exist:

```bash
ls -l /dev/fuse
fusermount3 -V
```

`/dev/fuse` is often world-read/write. If open fails with `EACCES`, add yourself to the `fuse` group and log in again:

```bash
sudo groupadd -f fuse
sudo usermod -aG fuse "$USER"
```

You do not need `user_allow_other` in `/etc/fuse.conf`. ArkFS does not pass `allow_other`.

```bash
make test                    # cargo test (debug) + mix test
make sim                     # Elixir scenario catalog (escript; needs Mix)
make release                 # target/release/arkfs only
sudo make install            # /usr/local/bin/arkfs (no Mix)
# or: make install PREFIX=/usr DESTDIR=/tmp/stage

mkdir -p /var/lib/arkfs /mnt/ark
arkfs mount --data /var/lib/arkfs /mnt/ark
# other terminal:
ls /mnt/ark
arkfs umount /mnt/ark
```

Without installing: `cargo run -p arkfs -- mount --data /var/lib/arkfs /mnt/ark`.

### macOS

Install [macFUSE](https://macfuse.github.io/) (kernel extension; not Linux `fuse3`):

```bash
brew install --cask macfuse
```

Then allow the system extension in **System Settings → Privacy & Security** (a restart is common), and confirm `ls /Library/Filesystems/macfuse.fs`. Phase 0 does not mount on macOS. The cask is for local FUSE tooling and a later macOS facade.

### Windows

Install [WinFsp](https://winfsp.dev/) (kernel driver plus a FUSE-compatible user-mode API):

```powershell
winget install --id WinFsp.WinFsp
# or: choco install winfsp
```

Reboot or log on again, then `Get-Service WinFsp.Launcher`. Phase 0 does not mount on Windows. Dokany is a different stack and is not used here.

## What is in the tree

| Library | Role |
|---------|------|
| `arkfs_core` | Shared types and `FileAttributes`. Pure projections for FUSE, NFS, SMB3, WebDAV, and macOS. Only FUSE is mounted. |
| `simulation_harness` | Logical clock, delay presets, and a chaos log (Rust and Elixir). Not a cluster. |
| `persistent_object_store` | Content-addressed blobs. `put` fsyncs, collects acks, then publishes. The mount uses `NoPeers`. `LocalQuorum` is extra directories under `replicas/` for tests. |
| `temporal_core` | Versioned directory tree. Live lookup hides tombstones. `View::AsOf` returns the last version at or before the timestamp. |

| App | Role |
|-----|------|
| `sim_runner` | Elixir scenario catalog. It checks clock, delay, and the chaos log. Store and temporal tests are `cargo test`. |
| `arkfs` | Single-node FUSE mount (`arkfs mount --data DIR MOUNTPOINT`, optional `--as-of N`). See [docs/fuse.md](docs/fuse.md). |

Canonical metadata is a superset of what each protocol needs. Facades map through `arkfs_core::attr_map`. Details: [docs/attributes.md](docs/attributes.md).

## License

[MIT](LICENSE). Copyright (c) 2026 jrfranks.
