# Single-node FUSE mount

`arkfs` mounts a local ArkFS store on Linux via FUSE. Semantics live in
`temporal_core`; this is a protocol facade. Maintainers: [maintainer.md](maintainer.md)
covers the kernel request path, isolation, and live-test skip rules.

```bash
mkdir -p /var/lib/arkfs /mnt/ark
cargo run -p arkfs -- mount --data /var/lib/arkfs /mnt/ark
# another terminal:
ls /mnt/ark
arkfs umount /mnt/ark
```

`--data` is required and is the PersistentObjectStore directory (`OwnerOnly`
quorum: local fsync, no replicas).

## Never-delete

`unlink` / `rmdir` append a tombstone version. The name disappears from the live
tree. Historical content remains:

```bash
arkfs mount --data /var/lib/arkfs --as-of 3 /mnt/ark-then
```

`--as-of <logical>` is read-only. Logical time advances on each commit (create,
write/fsync, unlink, setattr, …).

## Layout

```
--data DIR/
  primary/objects/*.obj
  primary/anchors/temporal_index
```

CI tests the namespace and open-file cache without `/dev/fuse`. A live mount
needs `fuse3` (`fusermount3`) and access to `/dev/fuse`.

Conformance and CLI option reach run in `make ci`. The live FUSE scaffold (`tests/fuse_drive.rs`) mounts ArkFS, drives kernel ops, and checks the object store after each step; GitHub skips it (`CI=1`). Local pre-commit sets `ARKFS_REQUIRE_FUSE=1`. See [fuse-conformance.md](fuse-conformance.md).
