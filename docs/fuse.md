# Single-node FUSE mount

`arkfs` mounts a local ArkFS store on Linux via FUSE. Semantics live in
`temporal_core`; this is a protocol facade. Maintainers: [maintainer.md](maintainer.md)
covers the kernel request path, isolation, and live-test skip rules.

```bash
make release
sudo make install            # arkfs only; PREFIX=/usr/local; override PREFIX= / DESTDIR=
mkdir -p /var/lib/arkfs /mnt/ark
arkfs mount --data /var/lib/arkfs /mnt/ark
# another terminal:
ls /mnt/ark
arkfs umount /mnt/ark
```

Dev loop: `cargo run -p arkfs -- mount --data /var/lib/arkfs /mnt/ark`.

`--data` is required. The store is `open_isolated_store` (`NoPeers`,
`OwnerOnly`): local fsync, no `replicas/` directory.

`arkfs fsck --data DIR` re-hashes every CAS object and exits nonzero on
failure. Names longer than 255 bytes are `ENAMETOOLONG`. `mkdir`/`create`/`mknod`
apply the FUSE `umask`. Setgid directories pass their gid to new children.

## Never-delete

`unlink` / `rmdir` append a tombstone version. The name disappears from the live
tree. Historical content remains:

```bash
arkfs mount --data /var/lib/arkfs --as-of 3 /mnt/ark-then
```

`--as-of <logical>` is read-only. Logical time advances on each commit (create,
write/fsync, unlink, setattr, rename, …).

Directory rename moves every live descendant in one index persist. Hard links
share `file_id` (and content after write). `mknod` can create fifo/socket/device
nodes as well as regular files.

## Layout

```
--data DIR/
  primary/objects/*.obj
  primary/anchors/temporal_index
```

CI tests the namespace and open-file cache without `/dev/fuse`. A live mount
needs `fuse3` (`fusermount3`) and access to `/dev/fuse`.

Conformance and CLI option reach run in `make ci`. The live FUSE scaffold (`tests/fuse_drive.rs`) mounts ArkFS, drives kernel ops, and checks the object store after each step; GitHub skips it (`CI=1`). Local pre-push sets `ARKFS_REQUIRE_FUSE=1`. See [fuse-conformance.md](fuse-conformance.md). Linux/libfuse ABI notes (not a
test oracle): [conformance/fuse-abi.md](conformance/fuse-abi.md).
