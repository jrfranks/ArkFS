//! Pure argv parser. Never mounts, never looks at the filesystem.
//!
//! `umount` and `unmount` are aliases. `--data DIR` is required for `mount`
//! (no implicit `/var/lib/arkfs`). `--as-of LOGICAL` is a u64 logical tick.

use std::path::PathBuf;

/// Action implied by argv. [`parse`] never executes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Mount {
        data: PathBuf,
        as_of: Option<u64>,
        mountpoint: PathBuf,
    },
    Umount {
        mountpoint: PathBuf,
    },
    /// Scan CAS objects and re-hash (`PersistentObjectStore::verify_integrity`).
    Fsck {
        data: PathBuf,
    },
}

/// Stdout for `--help`; also appended to parse errors so users see the grammar.
pub fn usage() -> &'static str {
    "usage:\n  arkfs mount --data DIR [--as-of LOGICAL] MOUNTPOINT\n  arkfs umount MOUNTPOINT\n  arkfs fsck --data DIR\n  arkfs --help"
}

/// Parse argv **without** argv[0]. Never mounts or unmounts.
pub fn parse<I, S>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    if args.is_empty() {
        return Err(usage().into());
    }
    match args[0].as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "mount" => parse_mount(&args[1..]),
        "umount" | "unmount" => parse_umount(&args[1..]),
        "fsck" => parse_fsck(&args[1..]),
        other if other.starts_with('-') => Err(format!("unknown flag {other}\n{}", usage())),
        other => Err(format!("unknown command {other}\n{}", usage())),
    }
}

/// Parse `mount --data DIR [--as-of N] MOUNTPOINT`.
fn parse_mount(args: &[String]) -> Result<Command, String> {
    let mut data: Option<PathBuf> = None;
    let mut as_of: Option<u64> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--data" => {
                i += 1;
                let p = args.get(i).ok_or("--data requires a path")?;
                data = Some(PathBuf::from(p));
            }
            "--as-of" => {
                i += 1;
                let raw = args.get(i).ok_or("--as-of requires a logical timestamp")?;
                as_of = Some(raw.parse().map_err(|_| format!("invalid --as-of {raw}"))?);
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag {flag}\n{}", usage()));
            }
            other => rest.push(other.to_string()),
        }
        i += 1;
    }
    if rest.len() > 1 {
        return Err(format!(
            "unexpected extra argument {}\n{}",
            rest[1],
            usage()
        ));
    }
    let mountpoint = rest
        .first()
        .cloned()
        .ok_or_else(|| format!("mountpoint required\n{}", usage()))?;
    let data = data.ok_or_else(|| format!("--data DIR is required\n{}", usage()))?;
    Ok(Command::Mount {
        data,
        as_of,
        mountpoint: PathBuf::from(mountpoint),
    })
}

/// Parse `umount|unmount MOUNTPOINT`.
fn parse_umount(args: &[String]) -> Result<Command, String> {
    if args.first().map(String::as_str) == Some("-h")
        || args.first().map(String::as_str) == Some("--help")
    {
        return Ok(Command::Help);
    }
    if args.len() != 1 {
        return Err(format!("umount requires a single mountpoint\n{}", usage()));
    }
    Ok(Command::Umount {
        mountpoint: PathBuf::from(&args[0]),
    })
}

/// Parse `fsck --data DIR`.
fn parse_fsck(args: &[String]) -> Result<Command, String> {
    let mut data: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--data" => {
                i += 1;
                let p = args.get(i).ok_or("--data requires a path")?;
                data = Some(PathBuf::from(p));
            }
            flag if flag.starts_with('-') => {
                return Err(format!("unknown flag {flag}\n{}", usage()));
            }
            other => {
                return Err(format!("unexpected argument {other}\n{}", usage()));
            }
        }
        i += 1;
    }
    let data = data.ok_or_else(|| format!("--data DIR is required\n{}", usage()))?;
    Ok(Command::Fsck { data })
}

/// Arguments for `fusermount3` / `fusermount` (`-u <mountpoint>`).
pub fn fusermount_argv(mountpoint: &str) -> Vec<String> {
    vec!["-u".into(), mountpoint.into()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// -h / --help / help all parse as Help.
    #[test]
    fn help_flags() {
        let _g = arkfs_test_review::guard();
        assert_eq!(parse(["--help"]).unwrap(), Command::Help);
        assert_eq!(parse(["-h"]).unwrap(), Command::Help);
        assert_eq!(parse(["help"]).unwrap(), Command::Help);
        assert_eq!(parse(["mount", "--help"]).unwrap(), Command::Help);
    }

    /// mount without --data or mountpoint is an error naming the flag.
    #[test]
    fn mount_requires_data_and_mountpoint() {
        let _g = arkfs_test_review::guard();
        assert!(parse(["mount"]).unwrap_err().contains("--data"));
        assert!(parse(["mount", "--data"]).unwrap_err().contains("--data"));
        assert!(parse(["mount", "--data", "/d"])
            .unwrap_err()
            .contains("mountpoint"));
        assert!(parse(["mount", "/mnt"]).unwrap_err().contains("--data"));
    }

    /// --as-of is parsed; unknown flags and extra args fail.
    #[test]
    fn mount_as_of_and_unknown() {
        let _g = arkfs_test_review::guard();
        let c = parse(["mount", "--data", "/d", "--as-of", "3", "/m"]).unwrap();
        assert_eq!(
            c,
            Command::Mount {
                data: PathBuf::from("/d"),
                as_of: Some(3),
                mountpoint: PathBuf::from("/m"),
            }
        );
        assert!(parse(["mount", "--as-of", "x", "--data", "/d", "/m"])
            .unwrap_err()
            .contains("invalid --as-of"));
        assert!(parse(["mount", "--data", "/d", "--quiet", "/m"])
            .unwrap_err()
            .contains("unknown flag"));
        assert!(parse(["mount", "--data", "/d", "/m", "extra"])
            .unwrap_err()
            .contains("extra"));
        assert!(parse(["--wat"]).unwrap_err().contains("unknown flag"));
        assert!(parse(["explode"]).unwrap_err().contains("unknown command"));
        assert!(parse(Vec::<String>::new()).unwrap_err().contains("usage"));
    }

    /// umount/unmount aliases; arity must be 1.
    #[test]
    fn umount_aliases_and_arity() {
        let _g = arkfs_test_review::guard();
        assert_eq!(
            parse(["umount", "/m"]).unwrap(),
            Command::Umount {
                mountpoint: PathBuf::from("/m")
            }
        );
        assert_eq!(
            parse(["unmount", "/m"]).unwrap(),
            Command::Umount {
                mountpoint: PathBuf::from("/m")
            }
        );
        assert!(parse(["umount"]).unwrap_err().contains("umount"));
        assert!(parse(["umount", "/a", "/b"])
            .unwrap_err()
            .contains("single"));
        assert_eq!(fusermount_argv("/m"), vec!["-u", "/m"]);
    }

    /// fsck --data DIR; missing --data fails.
    #[test]
    fn fsck_requires_data() {
        let _g = arkfs_test_review::guard();
        assert_eq!(
            parse(["fsck", "--data", "/d"]).unwrap(),
            Command::Fsck {
                data: PathBuf::from("/d")
            }
        );
        assert!(parse(["fsck"]).unwrap_err().contains("--data"));
        assert!(parse(["fsck", "--quiet"])
            .unwrap_err()
            .contains("unknown flag"));
    }
}
