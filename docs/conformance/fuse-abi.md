# Linux FUSE / libfuse ABI inventory

This is **not** an ArkFS test oracle and **must not** be used to generate
ArkFS tests. ArkFS FUSE reachability (what we implement and which test
witnesses it) is [fuse-conformance.md](../fuse-conformance.md).

Assertions below are what a userspace FUSE server must satisfy against the
Linux FUSE ABI, libfuse, and the fuser `Filesystem` mapping. Most of INIT,
unique IDs, lookup counts, and reply framing are **fuser’s** job. ArkFS
code is `FuseFs` → `ArkSession`; do not add tests that only re-check fuser.

## ArkFS mapping

- Phase 0 talks to the kernel through fuser (`abi-7-31`). INIT negotiation
  and request unique IDs are not ArkFS-owned.
- Lookup-count / forget pairing is fuser + kernel. ArkFS never-delete means
  inodes are not destroyed when nlookup hits zero in a way that frees CAS
  bytes; tombstones stay in the temporal index.
- `ENOSYS` defaults in fuser are listed here as ABI facts. ArkFS still
  implements the ops in `IMPLEMENTED_FUSE_OPS` and tests them in
  `fuse_facade`.
- POSIX-looking behavior is asserted on `ArkSession`, not by replaying this
  file.

IPC / replication is [ipc.md](ipc.md). POSIX.1 XSH inventory (also not an
ArkFS oracle) is [posix-xsh.md](posix-xsh.md).

**Status: Partial.** Claims below are those that survived source check against
Linux `fuse.h` / `fuse(4)` / `fs/fuse`, libfuse, and fuser. Gaps and excluded
claims are at the end.

Citation keys (`[S1]` … `[S20]`) are listed under [Sources](#sources).

---

## MUST be true

### Session identity and `FUSE_INIT`

1. Each open of `/dev/fuse` is a distinct session. Resources created through
   one fd are not visible through another. `[S1]`
2. The daemon must retain that session’s negotiated protocol version and INIT
   flags for the life of the connection. `[S1]` `[S2]`
3. The connection lasts until the daemon dies or the last mount reference is
   released. Lazy unmount does not break it immediately. `[S1]`
4. `FUSE_INIT` is the first kernel request on the session. `[S2]`
5. Both sides send the protocol version they support. `[S2]`
6. When majors match, both sides use the **smaller** minor. `[S2]`
7. If the kernel major is newer than userspace, the daemon replies with only
   its own major; the kernel then issues a new INIT. `[S2]`
8. If the userspace major is newer, the daemon must fall back to the kernel’s
   major. `[S2]`
9. `conn.capable` / `conn.capable_ext` are read-only kernel support bits.
   `conn.want` / `conn.want_ext` are the bits the filesystem enables. `[S14]`
10. Only bits the kernel advertised in `capable(_ext)` may be set in
    `want(_ext)`. Want bits outside capable abort INIT. `[S14]`
11. Opting into POSIX ACLs in INIT forces kernel `default_permissions`. `[S15]`
12. `FUSE_ALLOW_IDMAP` is accepted only if `default_permissions` is already
    set (including via POSIX ACL processed earlier in the same INIT reply).
    Otherwise INIT fails. `[S15]`

### Request I/O, unique IDs, and replies

13. The daemon must `read` requests and `write` replies on the session fd.
    The read buffer must be at least `FUSE_MIN_READ_BUFFER` (8192). `[S1]` `[S17]`
14. `FUSE_FORGET` and `FUSE_BATCH_FORGET` require no error/data payload reply
    (`fuse_reply_none` / fuser `forget` has no reply argument). `[S5]` `[S7]` `[S9]`
15. `FUSE_INTERRUPT` does **not** cancel the original request. The kernel still
    requires a reply to the original unique. Userspace may ignore the interrupt
    or complete the original with `EINTR`. `[S5]`
16. At most one interrupt is issued per operation. Interrupts are delivered
    before other queued requests. After sending it, the kernel waits
    uninterruptibly for the original completion. `[S5]`
17. Ordinary in-flight request uniques are even. `FUSE_INTERRUPT` uniques are
    the original unique with bit 0 set (odd). `[S3]`
18. A userspace write with unique 0 is an unsolicited notification; the error
    field is the notify code. `[S3]`
19. When `FUSE_HAS_RESEND` is negotiated, unique bit 63 (`FUSE_UNIQUE_RESEND`)
    marks a resent request. `[S3]`
20. A success reply’s payload must be the size and layout the kernel prepared
    for that opcode. `[S17]`
21. Every implemented `fuse_lowlevel_ops` method except `init` and `destroy`
    must pass its `fuse_req_t` to one of that opcode’s documented valid reply
    functions. The handle stays valid until a reply is sent. `[S6]`
22. fuser `Filesystem` methods correspond to those ops and take matching
    `ReplyEntry`, `ReplyCreate`, `ReplyAttr`, `ReplyOpen`, `ReplyData`,
    `ReplyWrite`, `ReplyDirectory` / `ReplyDirectoryPlus`, `ReplyXattr`,
    `ReplyEmpty`, and similar types. `[S6]` `[S10]`
23. A successful entry is `reply.entry(ttl, attr, generation)` or
    `entry_with_ttls` (separate attr vs name TTL), matching
    `fuse_entry_param`’s `attr_timeout` and `entry_timeout`. Errors use
    `reply.error`. `[S10]`
24. `getxattr` / `listxattr` must use `reply.size()` when size is 0,
    `reply.data()` when the value fits, or `reply.error(ERANGE)` when it does
    not — the same split as `fuse_reply_xattr` vs `fuse_reply_buf` vs
    `ERANGE`. `[S10]`

Valid replies by opcode:

| Opcodes | Valid replies |
| --- | --- |
| lookup, mknod, mkdir, symlink, link | `fuse_reply_entry` or `fuse_reply_err` |
| create, tmpfile | `fuse_reply_create` or `fuse_reply_err` |
| getattr, setattr | `fuse_reply_attr` or `fuse_reply_err` |
| open, opendir | `fuse_reply_open` or `fuse_reply_err` |
| read | `fuse_reply_buf`, `fuse_reply_iov`, `fuse_reply_data`, or `fuse_reply_err` |
| write, write_buf, copy_file_range | `fuse_reply_write` or `fuse_reply_err` |
| readdir, readdirplus | `fuse_reply_buf`, `fuse_reply_data`, or `fuse_reply_err` |
| getxattr, listxattr | `fuse_reply_buf`, `fuse_reply_data`, `fuse_reply_xattr`, or `fuse_reply_err` |
| unlink, rmdir, rename, flush, release, releasedir, fsync, fsyncdir, setxattr, removexattr, access, setlk, flock, fallocate, syncfs | `fuse_reply_err` (0 = success) |
| forget, forget_multi, retrieve_reply | `fuse_reply_none` |

### Node IDs, entry replies, lookup/forget pairing

25. `header.nodeid` is the filesystem object being operated on. The root inode
    node ID is `FUSE_ROOT_ID` (1). `[S4]`
26. An entry reply’s `(nodeid, generation)` pair must be unique for the
    filesystem’s lifetime. `[S4]` `[S8]`
27. Nodeid 0 is not a live inode. A zero nodeid in an entry reply is `ENOENT`
    (optionally cacheable). `[S18]`
28. In lookup, ino 0 is a negative entry the kernel may cache for
    `entry_timeout`. Returning `ENOENT` is also negative but not cacheable
    that way. `[S8]` `[S18]`
29. `attr` in a lookup/entry reply must be correct even if `attr_timeout` is
    0: the kernel uses lookup `st_size` for later reads. `[S8]`
30. If inodes are reused **and** the filesystem is NFS-exported, each reused
    ino must get a new unused generation so `(ino, generation)` stays unique
    for the filesystem lifetime. `[S8]`
31. Successful `fuse_reply_entry` (lookup, mknod, mkdir, symlink, link) and
    `fuse_reply_create` increment the inode lookup count by one. `[S7]` `[S20]`
32. `forget` / `forget_multi` decrease that count by `nlookup`. `[S7]` `[S20]`
33. `readdirplus` increments the lookup count of every returned entry except
    “.” and “..”. `readdir` does not change lookup counts. `[S7]`
34. Inodes with a non-zero lookup count may still receive kernel requests after
    unlink, rmdir, or overwrite-rename. The filesystem must keep serving them
    and must postpone destroying the inode until the lookup count reaches
    zero. `[S9]` `[S20]`
35. Unlink / rmdir / rename are followed closely by forget unless the file or
    directory is still open, in which case forget comes after `release` /
    `releasedir`. `[S9]`
36. On unmount, lookup counts implicitly drop to zero; corresponding forget
    messages are **not** guaranteed. `[S9]`

### High-level ops, permissions, locking, `ENOSYS`

37. High-level `fuse_operations` handlers must return the negated error code
    (`-errno`) directly rather than setting `errno`. (Low-level
    `fuse_reply_err` takes a **positive** errno.) `[S11]` `[S6]`
38. All high-level `fuse_operations` methods are optional. `[S12]`
39. `open`, `flush`, `release`, `fsync`, `opendir`, `releasedir`, `fsyncdir`,
    `access`, `create`, `truncate`, `lock`, `init`, and `destroy` may be
    omitted and a full-featured filesystem can still be implemented. `[S12]`
40. Undefined high-level methods yield `-ENOSYS` except `open`, `release`,
    `opendir`, `releasedir`, and `statfs`, which succeed with 0. `[S12]`
41. Permission checking is the filesystem’s job unless it opts into
    `default_permissions`. Then the kernel performs UNIX mode/owner checks
    first and userspace methods run only after that check succeeds. `[S13]`
42. Filesystems that implement no permission checks should set
    `default_permissions`. Enabling POSIX ACLs also turns
    `default_permissions` on. `[S13]` `[S15]`
43. If the daemon advertises POSIX or flock locking, it must actually service
    `GETLK` / `SETLK`. `[S19]`

`ENOSYS` is not a uniform “unsupported” error:

| Condition | Meaning of `ENOSYS` |
| --- | --- |
| `open` / `opendir` with `FUSE_NO_OPEN_SUPPORT` advertised | success; the kernel stops sending those ops `[S16]` |
| `open` without `FUSE_CAP_NO_OPEN_SUPPORT` | error to the caller |
| `ACCESS` | permanent success |
| `CREATE` | permanent fallback to mknod+open |
| `FSYNC` | success; future fsyncs are skipped |
| `open` when the kernel offers zero-message open, or `POLL` | permanent no-op of that opcode, not a one-shot skip while still claiming the capability `[S19]` |

---

## MUST be false

### Session and INIT

1. Two opens of `/dev/fuse` share one filesystem or one inode namespace. `[S1]`
2. INIT version or flags can be forgotten, skipped, or renegotiated ad hoc
   after the session is live. `[S1]` `[S2]`
3. Mismatched majors may stay in use. `[S2]`
4. When majors match, the **larger** minor is used. `[S2]`
5. `want` may include bits absent from `capable`. `[S14]`
6. `FUSE_ALLOW_IDMAP` can be enabled without `default_permissions`. `[S15]`

### Request I/O and replies

7. The daemon may use a read buffer smaller than 8192. `[S1]`
8. An implemented request (other than `init` / `destroy` / forget-family) may
   be left unreplied. `[S6]`
9. `FORGET` / `BATCH_FORGET` may be answered with a kernel error payload.
   `[S7]` `[S9]`
10. Unique 0 completes an in-flight kernel request. `[S3]`
11. Ordinary ops may use odd uniques, or interrupts even uniques. `[S3]`
12. `FUSE_INTERRUPT` cancels the original unique without a reply to that
    unique. `[S5]`
13. A success payload’s type or length may disagree with the opcode the kernel
    issued. `[S17]`
14. A missing unique on `write(2)` of a reply is reported as `EINVAL`. The
    kernel returns `-ENOENT` when the unique is not on the processing list.
    `[S17]` (excluded exact-`EINVAL` claim; see [Coverage](#coverage-and-uncertainty))

### Node IDs and lookup counts

15. Nodeid 0 is a live inode. `[S18]`
16. A newly created object’s nodeid may equal `FUSE_ROOT_ID` (1). `[S4]`
17. `(nodeid, generation)` may be reused while any client (including NFS) can
    still name the old object. `[S8]`
18. Lookup `attr` may be dummy when `attr_timeout` is 0. `[S8]`
19. `readdir` adjusts lookup counts. `[S7]`
20. `readdirplus` increments lookup count for “.” or “..”. `[S7]`
21. An inode may be destroyed while `nlookup > 0`. `[S9]` `[S20]`
22. Unmount is guaranteed to deliver matching forgets. `[S9]`

### High-level ops, permissions, `ENOSYS`

23. High-level handlers report errors by setting `errno` and returning a
    non-negated value. `[S11]`
24. Every `fuse_operations` method is required. `[S12]`
25. Every omitted method is `-ENOSYS`. `[S12]`
26. Userspace may skip permission checks by default (without
    `default_permissions` or equivalent). `[S13]`
27. Lock capabilities may be advertised without servicing `GETLK` / `SETLK`.
    `[S19]`
28. `ENOSYS` is a one-shot “not this time” while the capability remains
    claimed. `[S16]` `[S19]`
29. `ENOSYS` is always a permanent per-connection switch for every opcode.
    Lookup / getattr / read / write and many others return `ENOSYS` for that
    request only. `[S19]` (see [Coverage](#coverage-and-uncertainty))

---

## Coverage and uncertainty

Research status is **Partial**. The following are **not** asserted as
conformance requirements:

- **`FUSE_INTERRUPT` reply to the interrupt unique.** `fuse(4)` says
  `FUSE_INTERRUPT` “requires no response.” Kernel `fuse.rst` says that if the
  original request cannot be found, the daemon should reply to the INTERRUPT
  itself with `EAGAIN`. Both still require a reply to the **original** unique.
  `[S5]`
- **Header layout vs man page.** `fuse(4)` still documents `fuse_in_header`’s
  last field as `uint32_t` padding and the highest kernel protocol as 7.26.
  Current uapi (7.46) uses `uint16_t total_extlen` plus padding (since 7.38)
  and a larger `fuse_init_out`. `[S2]`
- **INIT enablement operator.** ABI comments do not specify a bitwise AND.
  The kernel enables features from flags returned in `fuse_init_out` (and
  `flags2` when `FUSE_INIT_EXT` is set). `[S14]` `[S15]`
- **Unique reuse after completion.** Implied by “unique identifier for this
  request” and lookup failure on a wrong unique; neither `fuse(4)` nor
  `fuse.h` states an explicit reuse-after-completion rule. `[S3]`
- **fuser lookup-count docs.** fuser `Filesystem::lookup` / `ReplyEntry` do
  not themselves state that a successful entry reply increments the kernel
  lookup count; that side effect is documented in libfuse. `[S7]` `[S10]`
- **fuser opcode gaps (0.15 / 0.18).** fuser has no `tmpfile`, `statx`,
  `write_buf`, `retrieve_reply`, `flock`, or `syncfs` methods, so those
  libfuse opcodes have no fuser equivalent. `[S10]`
- **NFS generation uniqueness** is required only if inodes are reused **and**
  the filesystem is NFS-exported, not for every local FUSE server. `[S8]`
- **High-level vs low-level errno sign.** Low-level `fuse_reply_err(req, err)`
  takes a positive errno and sends `-err` on the wire. The high-level
  “return `-errno`” rule does not apply to `fuse_lowlevel_ops` (or fuser).
  `[S11]` `[S6]`
- **Full `ENOSYS` catalog.** Kernel remaps (`EOPNOTSUPP` for xattr, `EINVAL`
  for rename2, `EOPNOTSUPP` for tmpfile, etc.) are larger than the table
  above. Only INIT, permission, open/create/access/fsync/poll cases are
  asserted. `[S16]` `[S19]`
- **Poll capability bit.** There is no FUSE INIT flag named for poll.
  “Advertising poll” is implied by not returning the documented `ENOSYS`
  permanent-success, not by a bit analogous to `FUSE_POSIX_LOCKS`. `[S19]`
- **`ENOSYS` after a prior success.** The kernel does not appear to reject
  that as a hard protocol error; it still latches `no_*` flags. A conformant
  server must not do it, but it is an inference, not a documented universal
  contract.
- **Unlimited-lifetime inodes.** libfuse allows ignoring forget if inodes
  have unlimited lifetime; pairing is mandatory only if the server uses
  `nlookup` to free objects. `[S20]`
- **High-level positive returns.** `fuse.h` says methods “should” return
  `-errno`. libfuse maps a positive return to `-ERANGE` in
  `fuse_send_reply_iov_nofree`, not in `fuse_operations` itself. `[S11]`
- **fuser default errors are not uniformly `ENOSYS`.** `open`/`opendir`
  default to success; `release`/`releasedir` default to `ok()`; `statfs`
  returns dummy stats; `symlink`/`link` default to `EPERM`. `[S10]` `[S12]`

### Claims dropped in verification

- A reply `write(2)` with an unknown unique is **not** specified as `EINVAL`;
  the kernel returns `-ENOENT` when the unique is missing from the processing
  list. Extra bytes on an error reply **are** `-EINVAL` (`oh.len != nbytes`).
  `[S17]`
- `FUSE_FORGET` is **not** the only no-reply opcode: `FUSE_BATCH_FORGET` and
  (per `fuse(4)`) `FUSE_INTERRUPT` are also no-payload. `[S5]`
- `ENOSYS` is **not** always a permanent per-connection switch. `[S16]` `[S19]`

---

## Sources

- `[S1]` [fuse(4)](https://man7.org/linux/man-pages/man4/fuse.4.html)
- `[S2]` [uapi linux/fuse.h](https://raw.githubusercontent.com/torvalds/linux/master/include/uapi/linux/fuse.h)
- `[S3]` [fs/fuse/fuse_dev_i.h](https://raw.githubusercontent.com/torvalds/linux/master/fs/fuse/fuse_dev_i.h), [fs/fuse/dev.c](https://raw.githubusercontent.com/torvalds/linux/master/fs/fuse/dev.c)
- `[S4]` uapi `fuse.h` (`FUSE_ROOT_ID`, `fuse_entry_out`)
- `[S5]` fuse(4); [Documentation/filesystems/fuse/fuse.rst](https://raw.githubusercontent.com/torvalds/linux/master/Documentation/filesystems/fuse/fuse.rst)
- `[S6]` [libfuse fuse_lowlevel.h](https://raw.githubusercontent.com/libfuse/libfuse/master/include/fuse_lowlevel.h); fuser `Filesystem`
- `[S7]` libfuse forget / readdirplus / reply docs
- `[S8]` [fuse_entry_param](https://libfuse.github.io/doxygen/structfuse__entry__param.html); fuser `Generation`
- `[S9]` libfuse forget/unlink/rmdir/rename; fuser `Filesystem::forget`
- `[S10]` fuser `ReplyEntry` / `ReplyXattr`; libfuse getxattr
- `[S11]` [libfuse fuse.h](https://raw.githubusercontent.com/libfuse/libfuse/master/include/fuse.h) (`fuse_operations`)
- `[S12]` libfuse `fuse.h` optional methods
- `[S13]` libfuse `fuse.h`, `mount.fuse3(8)`; Linux `fs/fuse/dir.c` `fuse_permission`
- `[S14]` libfuse `fuse_common.h` and `lib/fuse_lowlevel.c` (INIT)
- `[S15]` libfuse `fuse_common.h`; Linux `fs/fuse/inode.c` `process_init_reply`
- `[S16]` Linux `fs/fuse/file.c` and `dir.c` (`ENOSYS` / `no_open`)
- `[S17]` Linux `fs/fuse/dev.c` (`fuse_copy_out_args` / `fuse_dev_do_write`)
- `[S18]` Linux `fs/fuse/dir.c` (LOOKUP nodeid 0 → `ENOENT`)
- `[S19]` libfuse `fuse_lowlevel.h` (open/poll `ENOSYS`; POSIX lock ops)
- `[S20]` libfuse forget / `fuse_reply_entry` lookup-count pairing
