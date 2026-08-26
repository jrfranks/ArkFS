//! Command-option reach: every documented flag and the common mistake paths.
//!
//! Unit tests in `src/cli.rs` cover parse tables. This file execs the real
//! binary (`CARGO_BIN_EXE_arkfs`) so help text, exit codes, and stderr stay
//! wired. It must not try to mount (GitHub has no `/dev/fuse` guarantee).

use arkfs::cli::{parse, usage, Command};
use std::path::PathBuf;
use std::process::Command as Proc;

fn bin() -> Proc {
    Proc::new(env!("CARGO_BIN_EXE_arkfs"))
}

#[test]
fn binary_help_exits_zero() {
    let out = bin().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("arkfs mount"));
    assert!(text.contains("--data"));
    assert!(text.contains("--as-of"));
    assert!(text.contains("umount"));
}

#[test]
fn binary_no_args_exits_nonzero_and_prints_usage() {
    let out = bin().output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("usage"));
}

#[test]
fn binary_unknown_flag_mentions_usage() {
    let out = bin().args(["mount", "--quiet"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown flag"));
    assert!(err.contains(usage().lines().next().unwrap_or("usage")));
}

#[test]
fn parse_covers_documented_surface() {
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
}
