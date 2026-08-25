# ArkFS Canonical File Attributes

ArkFS stores a single **canonical** `FileAttributes` record per file version (cactus-stack snapshot). Protocol facades (**FUSE, NFS v3/v4, SMB 3, WebDAV, macOS**) are pure projections via `arkfs_core::attr_map`. No facade is source of truth.

## Lossless update rules

1. **Read-modify-write** the full canonical record on every setattr-style op.
2. Protocol patches use `Option` fields: only set fields change; others are preserved.
3. Facades may **omit** wire-inexpressible fields; they must not **clear** them.
4. Historical `lookup_at_timestamp` returns the full attribute snapshot at that logical time.

## Field matrix

| Canonical field | FUSE | NFS | SMB 3 | WebDAV | macOS |
|-----------------|------|-----|-------|--------|-------|
| `file_id` / generation | ✓ ino | ✓ fileid | ✓ FileId | partial | ✓ |
| `file_type` | ✓ | ✓ | ✓ | collection | ✓ |
| `mode` / nlink / uid / gid | ✓ | ✓ | partial | unix props | ✓ |
| `owner_name` / `group_name` | — | ✓ NFSv4 | ✓ | — | ✓ |
| `logical_size` / `allocation_size` | size/blocks | size/space_used | EOF/alloc | contentlength | ✓ |
| `atime` `mtime` `ctime` `btime` | ✓ (btime via xattr/ext) | ✓ time_create | 4 times | lastmod/creation | ✓ birthtime |
| `change_attr` | — | ✓ change | cache | etag related | — |
| `dos.*` | — | — | ✓ | — | mirrored hidden |
| `macos.*` | — | — | — | — | ✓ UF_/SF_ |
| `acl` | POSIX ACL / xattr | NFSv4 ACL | DACL | — | NFSv4-style |
| `security_descriptor` | — | — | ✓ | — | — |
| `xattrs` | ✓ | ✓ | EA | dead props | ✓ com.apple.* |
| `streams` (ADS / resource fork) | — | — | ✓ ADS | — | resource fork |
| `symlink_target` / reparse | ✓ | ✓ | reparse | — | ✓ |
| `content_type` / `etag` | — | — | — | ✓ live | — |
| `dead_props` | — | — | — | ✓ | — |
| `attr_checksum` | internal | internal | internal | internal | internal |

Legend: **✓** expressed; **partial** subset; **—** not on wire (retained in canonical store).

## API entry points

| Protocol | Project | Merge |
|----------|---------|-------|
| FUSE | `attr_map::to_fuse` | `merge_from_fuse` |
| NFSv4 | `to_nfs4` | `merge_from_nfs4` |
| SMB 3 | `to_smb3` | `merge_from_smb3` |
| WebDAV | `to_webdav_props` | `merge_from_webdav` |
| macOS | `to_macos` | `merge_from_macos` |

## Named streams

| Stream name | Meaning |
|-------------|---------|
| `NamedStream::PRIMARY` (`::$DATA`) | Primary content |
| `:…` | SMB alternate data stream |
| `com.apple.ResourceFork` | macOS resource fork |

Stream payloads are separate objects in PersistentObjectStore; sizes live on `NamedStream`.
POSIX setattr-style updates go through `FileAttributes::apply_posix` / `set_logical_size`
so FUSE, NFS, and SMB do not duplicate size/mode/uid/gid/time loops.

Attribute records persist via `arkfs_core::codec` (full field set + BLAKE3 trailer).
