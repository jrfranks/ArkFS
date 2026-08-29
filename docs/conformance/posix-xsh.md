# POSIX.1 System Interfaces (XSH) inventory

This is **not** an ArkFS test oracle and **must not** be used to generate
ArkFS tests. It is a portable POSIX.1 XSH inventory for comparison.

ArkFS POSIX-facing tests live in `fuse_facade` (`tests/conformance.rs` and
`src/session_tests.rs`). Kernel/libfuse programming rules (also not an
oracle) are [fuse-abi.md](fuse-abi.md). ArkFS FUSE reachability is
[fuse-conformance.md](../fuse-conformance.md).

Portable assertions for POSIX filesystem function definitions: what **must**
be true, what **must** be false, and what is **ambiguous** (not required on
every conforming implementation).

## ArkFS deviations

ArkFS is a **never-delete temporal filesystem**. POSIX claims in this file
that contradict that product rule are **not** ArkFS requirements.

| POSIX claim in this file | ArkFS |
| --- | --- |
| `unlink` / last `close` free the file’s space when link count hits 0; the file is no longer accessible; `st_ino` may be reused | Unlink appends a **tombstone**. Object bytes stay in the CAS. `--as-of` still reads them. `file_id` is not recycled for GC. |
| Directory-entry remove is the only history of the name | The temporal index keeps the full cactus of versions. |
| Successful `write` durability without `fsync` is not required | ArkFS `put` still fsyncs + quorum before success. Facade `write` is an open-file cache until flush/release persist. |
| Linux `*xattr(2)` is outside POSIX | ArkFS implements xattr on `ArkSession` anyway; tests are in `fuse_facade`. |

Do not derive a suite from the `unlink` / `close` space-freeing MUSTs. That
would fight never-delete.

**Status: Partial.** Core open/create/unlink/rename/read/write/lseek pages
were inventoried from POSIX.1-2024. Several metadata pages (`chmod`, `chown`,
`stat` extra ERRORS, `posix_fallocate`, `mmap` detail) were not fetched
page-by-page; see [Coverage](#coverage-and-uncertainty).

Citation keys (`[S1]` … `[S24]`) are listed under [Sources](#sources).

Primary text: POSIX.1-2024 (IEEE Std 1003.1-2024 / Issue 8, XSH tags
`tag_17_N`). Issue 7 (IEEE Std 1003.1-2017, `tag_16_N`) is the same core API
except where Issue 8-only or obsolescent items are called out.

---

## How to classify each function

Portable assertions come from each XSH Chapter 3 NAME/SYNOPSIS/DESCRIPTION
entry’s conformance-bearing sections: SYNOPSIS, DESCRIPTION, RETURN VALUE,
and ERRORS. `[S1]` `[S7]` `[S13]`

| Kind of text | Portable assertion? |
| --- | --- |
| Unshaded “shall” success, postcondition, or “shall fail” | **Must be true** on every conforming implementation |
| “Shall fail” condition is the only described error present | Any “the call succeeded” claim is **must be false**; the implementation shall not substitute a different `errno` for that described condition |
| Several errors apply at once | Any one of them may be returned; a claim that one particular code **must** win is **not** portable |
| “should”, “may”, “unspecified”, “implementation-defined”, “may fail” | **Ambiguous** — does not yield an assertion that must hold everywhere |
| Option-shaded text (FSC, SIO, XSI, OB) and Issue 8-only names | **Not** mandatory on every Issue 7 or unoptioned implementation |

After success, `errno` is unspecified unless that function’s own page says it
shall not be modified — and the `mkdir`, `rmdir`, `unlink`, `rename`, `open`,
`read`, `write`, `link`, `symlink`, and `stat` pages do not. No function shall
set `errno` to zero. `[S9]`

---

## Inventoried function definitions

Both issues specify `open`/`openat`, `creat`, `close`, `pread`/`read`,
`pwrite`/`write`, and `lseek`. Issue 8 additionally standardizes
`posix_close`, `O_CLOFORK`/`FD_CLOFORK`, and `lseek` `SEEK_HOLE`/`SEEK_DATA`.
`[S2]`

Directory-entry calls: `mkdir`/`mkdirat`, `rmdir`, `unlink`/`unlinkat`,
`rename`/`renameat`, `link`/`linkat`, `symlink`/`symlinkat`,
`readlink`/`readlinkat`. `[S3]`

Metadata: `fstat`; `fstatat`/`lstat`/`stat`; `chmod`/`fchmodat`; `fchmod`;
`chown`/`fchownat`; `fchown`; `lchown`; `futimens`/`utimensat` (and XSI
`utimes`); `truncate`; `ftruncate`; `access`/`faccessat`. `[S4]`

Also specified: `fdopendir`/`opendir`, `readdir` (`readdir_r` obsolescent in
Issue 8), `closedir`, Issue 8-only `posix_getdents`, `fcntl` (FD flags,
process-owned record locks, and in Issue 8 OFD locks plus `F_DUPFD_CLOFORK`),
`mmap`/`munmap`, `fsync` (FSC) and `fdatasync` (SIO), `fstatvfs`/`statvfs`,
XSI `mknod`/`mknodat`, `mkfifo`/`mkfifoat`. `[S5]`

Neither issue standardizes `getxattr`/`setxattr`/`listxattr`/`removexattr`
(or `attr_get`). Issue 8 leaves `O_XATTR`-style extended-attribute access
unstandardized. `[S6]`

---

## Cross-cutting postconditions

These apply on top of the per-function lists.

**Must be true**

1. Directory-modifying operations shall be atomic, serializable, and
   all-or-nothing. `[S11]`
2. If two threads each call one of `chmod`, `chown`, `creat`, `fchmod*`,
   `fchown*`, `fstat*`, `ftruncate`, `futimens`, `lchown`, `link*`, `lstat`,
   `open*`, `readlink*`, `rename*`, `stat`, `symlink*`, `truncate`,
   `unlink*`, `utimensat`, or `utimes`, each call shall see all of the
   specified effects of the other or none of them. `[S11]`
3. A second all-or-nothing list covers FD operations including `close`,
   `lseek`, `open`, `read`, `write`, and `writev` (“except where specified
   otherwise”). `[S11]`
4. File timestamp resolution is implementation-defined but no coarser than
   one second. Marked timestamps shall be updated on last-close and before a
   successful `fstat`, `fstatat`, `fsync`, `futimens`, `lstat`, `stat`,
   `utimensat`, or `utimes`. `[S20]`

**Must be false**

5. A “mark for update” clause requires an **immediately** visible new
   timestamp. XBD 4.12 allows deferred update until last-close or before a
   successful stat/fsync/utimens family call. `[S20]`
6. XSH 2.9.7 by itself forces whole-buffer atomicity of `read`/`write` on
   every file type. `write()` gives extra atomicity only for pipe/FIFO
   writes of `{PIPE_BUF}` bytes or less. `[S19]`

**Ambiguous**

7. When timestamps are updated other than last-close / before-stat-or-sync:
   unspecified. `[S20]`
8. When several “shall fail” conditions hold at once, which `errno` is
   returned is unspecified. Success is still false; a named-code winner is
   not portable. `[S13]`
9. A pathname beginning with two slashes may be interpreted in an
   implementation-defined manner. Follow limit is implementation-defined but
   at least `{SYMLOOP_MAX}`. `[S21]`
10. `{PATH_MAX}` `ENAMETOOLONG` is “may fail”; a component longer than
    `{NAME_MAX}` is “shall fail”. `[EILSEQ]` for a non-portable last
    component is “shall fail” only if the name cannot be created in the
    target directory (implementation-dependent). `[S16]`

---

## Per-function assertions

### `open`, `openat`, `creat`

**Must be true**

1. On success these functions open the file and return a non-negative file
   descriptor. `[S8]`
2. `creat(path, mode)` is `open(path, O_WRONLY|O_CREAT|O_TRUNC, mode)`. `[S8]`
3. With `O_CREAT|O_EXCL`, the existence check and create shall be atomic
   with respect to other threads doing the same `open` of the same name in
   the same directory. `[S11]`

**Must be false**

4. A −1 return creates or modifies any files. `[S10]`
5. Success when the named file is a directory and `oflag` includes
   `O_WRONLY` or `O_RDWR`, or includes `O_CREAT` without `O_DIRECTORY`.
   Required error: `EISDIR`. `[S14]`
6. Success for a non-directory prefix, `O_DIRECTORY` on a non-directory, or
   trailing slashes on a non-directory when `O_CREAT|O_EXCL` are not set.
   Required error: `ENOTDIR`. `[S14]`
7. A pathname with trailing slashes resolving successfully unless the last
   component is, or is being created as, a directory. `[S14]`
8. Success of `O_CREAT|O_EXCL` if the file exists (`EEXIST`), including if
   `path` names a symbolic link regardless of the link contents. `[S15]`
9. Success without `O_CREAT` when a component is missing or `path` is empty
   (`ENOENT`). With `O_CREAT`, success when a prefix is missing or `path`
   is empty (`ENOENT`). `[S16]`
10. `ENOENT` on a create with trailing slashes when the slash-stripped path
    names an existing file (then `ENOENT` shall not occur; `ENOTDIR` or
    `EEXIST` as applicable). `[S16]`
11. Success of a relative `*at` path whose `fd` is neither `AT_FDCWD` nor a
    valid reading/searching descriptor (`EBADF`). `[S18]`

**Ambiguous**

12. `O_CLOFORK` exists only in Issue 8.
13. Whether `openat` with an `O_XATTR` bit yields xattr access is not
    standardized. `[S6]`
14. Full matrix of `O_TRUNC` and flag combinations was not inventoried in
    this pass.

### `close`, `posix_close`

**Must be true**

1. `close` of a non-open descriptor shall fail `EBADF`. `[S24]`
2. When the link count is 0 and all file descriptors associated with the
   file are closed, the space occupied by the file shall be freed and the
   file shall no longer be accessible. `[S24]`

**Must be false**

3. `close` returning `EAGAIN` or `EWOULDBLOCK`.

**Ambiguous**

4. `posix_close` is Issue 8-only (`NAME` “close, posix_close”). Issue 7 has
   only `int close(int fildes)`.

### `read`, `pread`, `write`, `pwrite`

**Must be true**

1. On success these functions return a non-negative byte count that is
   never greater than `nbyte`. `[S8]` `[S12]`
2. After a successful `write()` to a regular file, a later successful
   `read()` of those byte positions shall return the written data until they
   are modified again, and a later successful `write()` to the same
   positions shall overwrite that data — requirements that apply to the
   file-system cache. `[S12]`
3. Cache entries shall be transferred to underlying storage as the result
   of successful `fdatasync()`, `fsync()`, or `aio_fsync()`. `[S12]`
4. An invalid fd not open for the requested access shall fail `EBADF`.
   `[S18]`
5. `pread`/`pwrite` shall fail `ESPIPE` if the file is incapable of seeking.
   `[S18]`

**Must be false**

6. A success assertion on those `EBADF`/`ESPIPE` shall-fail conditions when
   they are the only described error present. `[S13]`
7. Durability of a successful `write` **without** a successful
   `fsync`/`fdatasync`/`aio_fsync`. `[S19]`

**Ambiguous**

8. POSIX.1-2024 does not specify the interleaving of concurrent `write()`s
   to a regular file beyond per-call atomicity (applications are expected
   to use concurrency control). `[S19]`
9. `read()` rationale leaves some device types unspecified. `[S19]`

### `lseek`

**Must be true**

1. A bad fd shall fail `EBADF`. A pipe, FIFO, or socket shall fail
   `ESPIPE`. `[S23]`
2. Every seekable file has a virtual hole starting at current size. `[S23]`
3. Issue 8 adds `SEEK_HOLE` and `SEEK_DATA`. Issue 7 `lseek` has only
   `SEEK_SET`/`SEEK_CUR`/`SEEK_END`. `[S2]` `[S23]`

**Must be false**

4. Treating every run of zero bytes as a hole. A hole is a contiguous run
   of zeros, but not all zeros need belong to a hole. `[S23]`

**Ambiguous**

5. Hole creation and granularity are implementation-defined. `[S23]`
6. `SEEK_HOLE` may set the offset to the file size instead of an interior
   hole when `offset` falls beyond the last non-hole byte. `[S23]`
7. Not all file systems support holes. `[S23]`
8. `lseek` on non-seekable devices is implementation-defined and that file
   offset is undefined. `[SHM]`/`[TYM]` results are unspecified. `[S23]`

### `mkdir`, `mkdirat`

**Must be true**

1. On success these functions return 0 and the newly created directory
   shall be empty. `[S8]` `[S17]`

**Must be false**

2. If −1 is returned, a directory was created. `[S10]`
3. Success if `path` names a symbolic link, or the named file exists
   (`EEXIST`). `[S15]` `[S17]`
4. Success if a path-prefix component is missing or `path` is empty
   (`ENOENT`). `[S16]` `[S17]`

**Ambiguous**

5. Which of several concurrent errors is returned is not fixed. `[S13]`
6. `mkdir`/`mkdirat` are **not** in XSH 2.9.7’s hierarchy-atomicity list
   (XBD 4.4 still covers directory create). `[S11]`

### `rmdir`

**Must be true**

1. On success return 0. `[S8]`
2. If `path` names a symbolic link, `rmdir` shall fail `ENOTDIR`. `[S17]`
3. If the directory is not empty, `rmdir` shall fail and set `errno` to
   `EEXIST` or `ENOTEMPTY`. `[S17]`

**Must be false**

4. If −1 is returned, the named directory was changed. `[S10]`

**Ambiguous**

5. `EEXIST` versus `ENOTEMPTY` on a non-empty directory. `[S17]`
6. `EBUSY` only if the implementation considers an in-use directory an
   error. `[S17]`
7. `rmdir` is not in XSH 2.9.7’s hierarchy-atomicity list. `[S11]`

### `unlink`, `unlinkat`

**Must be true**

1. On success return 0: the named directory entry shall be removed and the
   link count decremented. `[S8]` `[S21]` `[S24]`
2. If the last link is removed while the file is still referenced, that
   **link** shall be removed before `unlink` returns. `[S24]`
3. If `path` names a symbolic link, `unlink` shall remove the symlink
   itself and shall not affect the file named by its contents. `[S21]`
4. Space occupied by the file shall be freed when the link count becomes 0
   **and** no process still references the file via an open file descriptor
   or `mmap`; until then removal of the contents is postponed. After the
   space is freed, `st_ino` shall become available for reuse. `[S24]`
5. `unlinkat` with `AT_REMOVEDIR` on a non-empty directory shall fail
   `EEXIST` or `ENOTEMPTY`. `[S17]`
6. An empty `path` or missing component shall fail `ENOENT`. `[S16]`

**Must be false**

7. If −1 is returned, the named file was changed (these functions shall
   fail and shall not unlink the file). `[S10]` `[S24]`

**Ambiguous**

8. Unlink of a directory is `EPERM` only if the process lacks appropriate
   privileges **or** the implementation prohibits unlinking directories.
   Unlink of a directory is **not** a must-fail on implementations that
   support it for a privileged process; POSIX still requires `EPERM` (not
   `EISDIR`) when it does fail, while noting LSB/Linux `EISDIR` as a
   portability conflict. `[S24]`
9. `EBUSY` is required only when the implementation considers the in-use
   object an error. `[S24]`
10. XSI-shaded sticky-bit `EPERM`/`EACCES` includes an
    implementation-defined case where `S_ISVTX` is set on a writable
    directory and the process can write the file. `S_ISVTX` on a
    non-directory is unspecified. `[S24]`
11. Any part of the path changing in parallel with `unlink` yields
    unspecified behavior. `[S24]`

### `rename`, `renameat`

**Must be true**

1. On success `rename` returns 0. `[S8]`
2. If `new` already exists it shall be removed and `old` renamed to `new`,
   and a directory entry named `new` shall remain visible to other threads
   throughout, referring either to the pre-operation `new` or `old` file.
   `[S11]` `[S14]`

**Must be false**

3. On failure, either the file named by `old` or the file named by `new`
   was changed or created, except: if `rename` fails for a reason other
   than `EIO`, any file named by `new` shall be unaffected. `[S10]` `[S14]`
4. Renaming a non-directory onto a directory (`EISDIR`). `[S14]`
5. Renaming a directory onto a non-directory (`ENOTDIR`). `[S14]`
6. `new` naming a non-empty directory (`EEXIST` or `ENOTEMPTY`). `[S17]`
7. Empty `old`/`new`, a nonexistent `old`, or a missing prefix of `new`
   (`ENOENT`). `[S16]`

**Ambiguous**

8. `EXDEV` is required only when the two sides are on different file
   systems **and** the implementation does not support cross-file-system
   hard links. `link()`/`rename()` may succeed across file systems if the
   implementation supports those links. `[S21]`
9. `EBUSY` only when the implementation considers the in-use directory an
   error. `[S17]`

### `link`, `linkat`

**Must be true**

1. On success return 0: the call shall atomically create a new hard link
   and increment the link count by one. `[S8]` `[S11]` `[S21]`

**Must be false**

2. If `link` fails, a link was created or the link count changed. `[S10]`
   `[S21]`
3. Success if `path2` resolves to an existing directory entry or refers to
   a symbolic link (`EEXIST`). `[S15]`
4. Create-with-trailing-slash `ENOENT`/`ENOTDIR` rules, including the ban
   on `ENOENT` when the slash-stripped path names an existing file. `[S16]`

**Ambiguous**

5. If `path1` names a symbolic link, whether `link` follows it or
   hard-links the symlink itself is implementation-defined. `[S21]`
6. `EXDEV` as for `rename`. `[S21]`
7. Last-component symlink following is function-specific. `[S21]`

### `symlink`, `symlinkat`, `mknod`, `mknodat`

**Must be true**

1. On success `symlink`/`symlinkat` return 0. `[S8]`
2. `mknod`/`mknodat` are XSI-shaded. `[S5]`

**Must be false**

3. `symlink` succeeding if `path2` names an existing file (`EEXIST`).
   `[S15]`
4. `mknod` succeeding if the named file exists, including when `path`
   names a symbolic link (`EEXIST`). `[S15]`
5. Create-with-trailing-slash `ENOENT`/`ENOTDIR` rules as for `open` /
   `link`. `[S16]`

**Ambiguous**

6. XSI `mknod` is not required without that option. `[S5]`
7. `mknod`/`mknodat` are not in XSH 2.9.7’s hierarchy-atomicity list.
   `[S11]`

### `stat`, `lstat`, `fstat`, `fstatat`

**Must be true**

1. On success these functions return 0. `[S8]`
2. `fstat` takes an open fd; `fstatat`/`lstat`/`stat` take a pathname
   (with `fstatat` flags). `[S4]`
3. They participate in the 2.9.7 all-or-nothing set and in the required
   timestamp-update-before-stat set. `[S11]` `[S20]`

**Must be false**

4. A −1 return is not a failure path (it must set `errno`). `[S9]`
5. Success-plus-zero-`errno` is required. `[S9]`

**Ambiguous**

6. Post-success `errno` preservation (these pages do not say `errno` shall
   not be modified). `[S9]`
7. Extra shall-fail rows on these pages were not fetched in this pass.

### Directory streams: `opendir`, `fdopendir`, `readdir`, `readdir_r`, `closedir`, `posix_getdents`

**Must be true**

1. A `DIR` stream is an ordered sequence of all entries in a particular
   directory. `[S22]`
2. `posix_getdents` is Issue 8-only. `[S5]` `[S22]`

**Must be false**

3. `readdir_r` is a current required interface. It is obsolescent (`[OB]`)
   and may be removed in a future version. `[S22]`

**Ambiguous**

4. POSIX does not specify the directory order. `[S22]`
5. Whether `readdir` returns an entry for a file added or removed after the
   last `opendir`/`rewinddir` is unspecified. `[S22]`
6. Using these functions in both parent and child after `fork` is
   undefined. `[S22]`
7. `seekdir` is XSI-shaded. `[S22]`

### `fcntl`, `mmap`, `munmap`, `fsync`, `fdatasync`, `fstatvfs`, `statvfs`, `mkfifo`, `mkfifoat`, remaining metadata

**Must be true**

1. `fcntl` documents `F_GETFD`/`F_SETFD`/`F_GETFL`/`F_SETFL` and
   process-owned `F_GETLK`/`F_SETLK`/`F_SETLKW`. Issue 8 adds OFD locks
   `F_OFD_GETLK`/`F_OFD_SETLK`/`F_OFD_SETLKW` and `F_DUPFD_CLOFORK`. `[S5]`
2. Successful `fsync`/`fdatasync` (when the FSC/SIO options apply)
   transfer cache entries to storage. `[S12]` `[S5]`
3. `chmod`/`fchmod`/`fchmodat`, `chown`/`fchown`/`fchownat`/`lchown`,
   `futimens`/`utimensat`, `truncate`/`ftruncate`, and
   `readlink`/`readlinkat` are in the 2.9.7 hierarchy atomicity set.
   `[S11]`
4. `mkfifo`/`mkfifoat` are specified special-file creates. `[S5]`
5. If a process has appropriate privileges, requested read, write, or
   directory-search access shall be granted. `[S18]`

**Must be false**

6. `EACCES` as a universal must-fail for a process with appropriate
   privileges. `[S18]`

**Ambiguous**

7. `fsync` without FSC, `fdatasync` without SIO, and XSI `utimes` are not
   required on every implementation. `[S5]`
8. Functions not required to read or write data or change status have
   unspecified effect on timestamps. `[S20]`
9. Extra shall-fail rows for `chmod`/`chown`/`access`/`truncate`/
   `utimensat`/`mmap`/`fcntl` locks were not fetched in this pass.

### Never POSIX-true for this API

There are no POSIX C function definitions for `getxattr`, `setxattr`,
`listxattr`, `removexattr`, or `attr_get`, so no POSIX shall/shall-fail
catalog exists for them. `O_XATTR` is explicitly not standardized. `[S6]`
Linux `*xattr(2)` is outside this document.

---

## Coverage and uncertainty

- Printed IEEE PDF clause numbers were not inspected; HTML identifiers used
  here are the XSH Chapter 3 tags plus the function NAME.
- Issue 7 synopses for most listed functions were not re-fetched
  page-by-page; grouping (`*at` companions on the same page) is from Issue
  8 pages plus the Issue 7 ToC, except `close()` which was compared
  directly.
- POSIX.1-2024 has no single labeled inventory of filesystem functions.
  XSH 2.9.7’s hierarchy-atomicity list omits `mkdir`/`mkdirat`, `rmdir`,
  `mkfifo`/`mkfifoat`, and `mknod`/`mknodat`, which XBD 4.4 still covers as
  directory create/unlink/rename examples.
- Issue 8 changed several clauses (`SEEK_HOLE`/`SEEK_DATA`; 2.9.7 broadened
  from regular files/symlinks to the file hierarchy; unlink last-close text
  clarified by Austin Group Defects 1314 and 1385). Issue 7 wording is not
  interchangeable.
- How Linux, BSD, NFS, or FUSE instantiate these POSIX latitudes is outside
  the standard text and was not verified from those implementations.

---

## Sources

- `[S1]` POSIX.1-2017 / POSIX.1-2024 XSH contents —
  https://pubs.opengroup.org/onlinepubs/9799919799/functions/contents.html
- `[S2]` XSH `open`/`creat`/`close`/`read`/`write`/`lseek`
- `[S3]` XSH `mkdir`/`rmdir`/`unlink`/`rename`/`link`/`symlink`/`readlink`
- `[S4]` XSH `fstat`/`fstatat`/`chmod`/`fchmod`/`chown`/`fchown`/`lchown`/`futimens`/`truncate`/`ftruncate`/`access`
- `[S5]` XSH `fdopendir`/`readdir`/`closedir`/`posix_getdents`/`fcntl`/`mmap`/`munmap`/`fsync`/`fdatasync`/`fstatvfs`/`mknod`/`mkfifo`
- `[S6]` XSH contents and `open()` rationale (`O_XATTR` not standardized)
- `[S7]` POSIX.1-2024 Base Definitions — 1. Introduction (Word Usage)
- `[S8]` RETURN VALUE on `mkdir`, `rmdir`, `unlink`, `rename`, `link`, `symlink`, `stat`, `open`, `read`, `write`
- `[S9]` `errno`; XSH 2.3 Error Numbers
- `[S10]` XSH 2.3; RETURN VALUE; XBD 4.4 Directory Operations
- `[S11]` XBD 4.4; XSH 2.9.7 Thread Interactions with Regular File Operations
- `[S12]` `write()`; XBD 4.11 File System Cache
- `[S13]` XSH 2.3 Error Numbers
- `[S14]` `open()`, `rmdir()`, `rename()`; XBD 4.16 Pathname Resolution
- `[S15]` `mkdir()`, `mknod()`, `symlink()`, `link()`, `open()` (`EEXIST`)
- `[S16]` `open()`, `unlink()`, `rmdir()`, `rename()`, `mkdir()`, `link()`, `symlink()`, `mknod()` (`ENOENT` / trailing slash)
- `[S17]` `rmdir()`, `rename()`, `unlink()`/`unlinkat()`, `mkdir()`, `open()`
- `[S18]` `close()`, `read()`, `write()`, `lseek()`, `link()`, `rename()`, `unlink()`; XBD 4.7 File Access Permissions
- `[S19]` `write()`; XSH 2.9.7; `unlink()` RATIONALE
- `[S20]` XBD 4.12 File Times Update
- `[S21]` `link()`; `unlink()`; XBD 4.16 Pathname Resolution
- `[S22]` `readdir()`; XBD 1.8.1 Codes (OB, XSI)
- `[S23]` `lseek()`; XBD 3.169 Hole
- `[S24]` `unlink()`; `close()`; XBD 4.5 Directory Protection

HTML pages: https://pubs.opengroup.org/onlinepubs/9799919799/functions/_name_.html
(Issue 8) and https://pubs.opengroup.org/onlinepubs/9699919799/functions/_name_.html
(Issue 7).
