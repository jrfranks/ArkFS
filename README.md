# ArkFS

[![CI](https://github.com/jrfranks/ArkFS/actions/workflows/ci.yml/badge.svg)](https://github.com/jrfranks/ArkFS/actions/workflows/ci.yml)

ArkFS is a never-delete filesystem. Pick a timestamp, and you get the tree as it was. Nothing is squashed, and nothing is gone.

The live path today is a single Linux node you can mount with FUSE. Rust owns the object store and the temporal index. Elixir runs the simulation harness (clock jumps, delay, bit flips, node loss). GitHub Actions runs the fast test bar. The slower proofs (Kani, Miri) run on `git push` if you have those tools installed.

This is Phase 0: one node, a real mount, and conformance tests. The longer product map is in [SPEC.md](SPEC.md). If you are going to change code, start with [docs/maintainer.md](docs/maintainer.md) (layout, request path, tests, traps). IPC oracle: [docs/conformance/ipc.md](docs/conformance/ipc.md).

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
| `arkfs_core` | Shared types and **FileAttributes** (FUSE / NFS / SMB3 / WebDAV / macOS) |
| `simulation_harness` | Clock, delay, chaos |
| `persistent_object_store` | Safe-write object store (fsync + quorum) |
| `temporal_core` | Cactus-stack temporal engine |

| App | Role |
|-----|------|
| `sim_runner` | Elixir harness scenarios. Store and temporal tests live in `cargo test`. |
| `arkfs` | Single-node FUSE mount (`arkfs mount --data DIR MOUNTPOINT`). See [docs/fuse.md](docs/fuse.md). |

Canonical metadata is a superset of what each protocol needs. Facades map through `arkfs_core::attr_map`. Details: [docs/attributes.md](docs/attributes.md).

## License

[MIT](LICENSE). Copyright (c) 2026 jrfranks.
