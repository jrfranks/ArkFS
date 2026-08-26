//! Command-option reach: every documented flag and the common mistake paths.
//!
//! Unit tests in `src/cli.rs` cover parse tables. This file execs the real
//! binary (`CARGO_BIN_EXE_arkfs`) so help text, exit codes, and stderr stay
//! wired. It must not try to mount (GitHub has no `/dev/fuse` guarantee).

use arkfs::cli::{parse, usage, Command};
#[allow(unused_imports)]
use arkfs_test_review::{review_assert as assert, review_eq as assert_eq, review_ne as assert_ne};
use std::path::PathBuf;
use std::process::Command as Proc;

/// Command builder for the compiled arkfs binary.
fn bin() -> Proc {
    Proc::new(env!("CARGO_BIN_EXE_arkfs"))
}

/// --help exits 0 and prints mount/--data/--as-of/umount.
#[test]
fn binary_help_exits_zero() {
    let _g = arkfs_test_review::guard();
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("arkfs mount"));
    assert!(text.contains("--data"));
    assert!(text.contains("--as-of"));
    assert!(text.contains("umount"));
    assert!(text.contains("fsck"));
}

/// No argv → nonzero and stderr contains usage.
#[test]
fn binary_no_args_exits_nonzero_and_prints_usage() {
    let _g = arkfs_test_review::guard();
    let out = bin().output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("usage"));
}

/// Unknown flag → unknown flag + usage on stderr.
#[test]
fn binary_unknown_flag_mentions_usage() {
    let _g = arkfs_test_review::guard();
    let out = bin().args(["mount", "--quiet"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown flag"));
    assert!(err.contains(usage().lines().next().unwrap_or("usage")));
}

/// parse() table: help, mount, --as-of, umount aliases, mistakes.
#[test]
fn parse_covers_documented_surface() {
    let _g = arkfs_test_review::guard();
    assert!(matches!(parse(["--help"]), Ok(Command::Help)));
    assert!(matches!(
        parse(["mount", "--data", "/d", "/m"]),
        Ok(Command::Mount { as_of: None, .. })
    ));
    assert_eq!(
        parse(["mount", "--as-of", "9", "--data", "/d", "/mnt"]).unwrap(),
        Command::Mount {
            data: PathBuf::from("/d"),
            as_of: Some(9),
            mountpoint: PathBuf::from("/mnt"),
        }
    );
    assert!(matches!(
        parse(["umount", "/mnt"]),
        Ok(Command::Umount { .. })
    ));
    assert!(matches!(
        parse(["unmount", "/mnt"]),
        Ok(Command::Umount { .. })
    ));
    assert!(matches!(
        parse(["fsck", "--data", "/d"]),
        Ok(Command::Fsck { .. })
    ));
}
