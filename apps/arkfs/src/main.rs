//! `arkfs` binary: parse argv, then mount (blocking) or fusermount3 -u.
//!
//! Mount errors print to stderr and exit 1. Help prints usage to stdout.
//! Requires Linux + fuse3. `--data` is the PersistentObjectStore root.

use arkfs::cli::{self, Command};
use fuse_facade::ArkSession;
use std::env;
use std::process::Command as Proc;

fn main() {
    match cli::parse(env::args().skip(1)) {
        Ok(Command::Help) => {
            println!("{}", cli::usage());
        }
        Ok(Command::Mount {
            data,
            as_of,
            mountpoint,
        }) => {
            if let Err(e) = do_mount(data, as_of, mountpoint) {
                eprintln!("arkfs: {e}");
                std::process::exit(1);
            }
        }
        Ok(Command::Umount { mountpoint }) => {
            if let Err(e) = do_umount(&mountpoint) {
                eprintln!("arkfs: {e}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("arkfs: {e}");
            std::process::exit(1);
        }
    }
}

fn do_mount(
    data: std::path::PathBuf,
    as_of: Option<u64>,
    mountpoint: std::path::PathBuf,
) -> Result<(), String> {
    let session = ArkSession::mount_store(&data, as_of).map_err(|e| e.to_string())?;
    eprintln!(
        "arkfs: mounting {} (data {}, {})",
        mountpoint.display(),
        data.display(),
        if session.read_only {
            "read-only as-of"
        } else {
            "read-write"
        }
    );
    fuse_facade::mount(session, &mountpoint).map_err(|e| e.to_string())
}

fn do_umount(mountpoint: &std::path::Path) -> Result<(), String> {
    let mp = mountpoint.to_str().ok_or("mountpoint is not UTF-8")?;
    let argv = cli::fusermount_argv(mp);
    let status = Proc::new("fusermount3")
        .args(&argv)
        .status()
        .or_else(|_| Proc::new("fusermount").args(&argv).status())
        .map_err(|e| format!("fusermount: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("fusermount failed: {status}"))
    }
}
