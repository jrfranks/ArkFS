## Summary

<!-- What changed and why. New maintainers: docs/maintainer.md. -->

## Testing

- [ ] `make ci` passes locally (fmt + package tests; no `/dev/fuse` required)
- [ ] If this touches FUSE kernel behavior, live `fuse_drive` was run (`ARKFS_REQUIRE_FUSE=1`)

## Checklist

- [ ] Behavior lives in the owning library, not in an app oracle or shared path
- [ ] Attribute updates are read-modify-write of the canonical record
- [ ] Tests cover the claim (including a failing case, not only the happy path)
- [ ] Single-node FUSE still behaves as if no peers are connected (no `replicas/` dirs, no network)
