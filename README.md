# ArkFS

Sovereign continuous temporal distributed file system. Data follows the owner (Earth → Moon → Mars). Never-delete with arbitrary timestamp access. Private clusters. Custom modular code (Elixir orchestration + Rust performance paths).

**New maintainers:** start at [docs/maintainer.md](docs/maintainer.md) (glossary, on-disk layout, request path, tests, traps). Product spec: [SPEC.md](SPEC.md). IPC test oracle: [docs/conformance/ipc.md](docs/conformance/ipc.md).

## Organization

**Make** is the monorepo organizer. Each deliverable is an **independent library** or an **app that includes libraries**. There is no Mix umbrella.

```
libs/rust/*     Rust libraries (Cargo workspace convenience under Make)
libs/elixir/*   Elixir libraries (each has its own mix.exs)
apps/*          Runnable apps that depend on libraries
```

## Quick start

```bash
git clone <url> ArkFS
cd ArkFS
make setup     # rustfmt, clippy, Kani, Miri, llvm-cov; enables git hooks
make test
```

`make setup` is idempotent. `make test` / `make build` also set `core.hooksPath` so the push gate is armed without a separate setup step. After hooks are on, `git checkout` / `git pull` re-run the installer. `make test` does not install Kani. GitHub sets `CI` and skips the installer (`make ci` only). Commits are not gated; `git push` runs `make prepush`. See [`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md).

## FUSE client

A live `arkfs` mount needs a FUSE **client** on the host (kernel module or equivalent plus the unmount helper). Phase 0 `arkfs mount` is **Linux-only** (`fuse3`, `/dev/fuse`, `fusermount3`). macOS and Windows steps install the platform FUSE stack so you can develop against it or run a FUSE filesystem when a non-Linux facade exists.

Mount docs: [docs/fuse.md](docs/fuse.md).

### Linux

Install FUSE 3 and the development headers used to build `fuser`:

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

Confirm the device and helper exist:

```bash
ls -l /dev/fuse
fusermount3 -V
```

`/dev/fuse` is often world-read/write. If open fails with `EACCES`, add your user to the `fuse` group and log in again:

```bash
sudo groupadd -f fuse
sudo usermod -aG fuse "$USER"
```

You do **not** need `user_allow_other` in `/etc/fuse.conf` for `arkfs` (the mount does not pass `allow_other`).

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

Dev loop without installing: `cargo run -p arkfs -- mount --data /var/lib/arkfs /mnt/ark`.

### macOS

Install **macFUSE** (kernel extension; not the same as Linux `fuse3`):

```bash
# Homebrew
brew install --cask macfuse
```

Or download the signed installer from [macfuse.github.io](https://macfuse.github.io/).

After install:

1. Open **System Settings → Privacy & Security** and allow the macFUSE system extension (you may need to restart).
2. Confirm the filesystem bundle exists: `ls /Library/Filesystems/macfuse.fs`.

Phase 0 `arkfs` does not mount on macOS. The cask is for local FUSE tooling and a future macOS facade.

### Windows

Install **WinFsp** (Windows File System Proxy). It provides the kernel driver and a FUSE-compatible user-mode API:

```powershell
# winget
winget install --id WinFsp.WinFsp

# Chocolatey
choco install winfsp
```

Or the MSI from [winfsp.dev](https://winfsp.dev/) / [github.com/winfsp/winfsp/releases](https://github.com/winfsp/winfsp/releases).

A reboot (or at least a new logon) is typical after the driver is installed. Confirm with:

```powershell
Get-Service WinFsp.Launcher
```

Phase 0 `arkfs` does not mount on Windows. WinFsp is the usual host stack for a later Windows facade. Dokany is a separate stack and is not used by this tree.

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
| `arkfs` | Single-node FUSE mount (`arkfs mount --data DIR MOUNTPOINT`). See [docs/fuse.md](docs/fuse.md). |

## Attribute model

Canonical metadata is a superset of protocol needs. Facades map via `arkfs_core::attr_map`. Details: [docs/attributes.md](docs/attributes.md).

## License

[MIT](LICENSE) — Copyright (c) 2026 jrfranks.
