//! CLI parsing for `arkfs`. Mount execution stays in the binary so this crate
//! can unit-test argv without touching `/dev/fuse`.
//!
//! Integration tests in `tests/cli_usability.rs` exec `CARGO_BIN_EXE_arkfs` and
//! check exit codes. User-facing mount docs: `docs/fuse.md`.

pub mod cli;

pub use cli::{parse, usage, Command};
