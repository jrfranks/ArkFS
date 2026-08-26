//! Structured test logs so a later automated review can see what each test did.
//!
//! Every `cargo test` appends NDJSON to
//! `target/arkfs-test-review/events.ndjson` (override with `ARKFS_TEST_REVIEW_DIR`).
//! Disable with `ARKFS_TEST_REVIEW=0`. No-op under Miri/Kani (those cannot
//! write the workspace `target/` tree).
//!
//! # In a test
//!
//! ```ignore
//! let _g = arkfs_test_review::guard();
//! arkfs_test_review::step("mkdir /d mode 0700");
//! // shadow std macros in the test module:
//! use arkfs_test_review::{review_assert as assert, review_eq as assert_eq};
//! ```

use std::fmt::Debug;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, Once};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static INIT: Once = Once::new();
static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

thread_local! {
    static STATE: std::cell::RefCell<Option<TestState>> = const { std::cell::RefCell::new(None) };
}

/// Per-thread test span. Drop writes `end` if the test did not already.
struct TestState {
    name: String,
    start: Instant,
    ended: bool,
}

impl Drop for TestState {
    fn drop(&mut self) {
        if !self.ended {
            self.ended = true;
            let ok = !thread::panicking();
            emit_end(&self.name, ok, self.start.elapsed().as_millis());
        }
    }
}

/// RAII span. Bind it (`let _g = guard()`) so Drop runs at the end of the test.
#[must_use]
pub struct Guard(());

/// Start (or nest) a review span for the current libtest thread name.
pub fn guard() -> Guard {
    ensure_started();
    Guard(())
}

impl Drop for Guard {
    fn drop(&mut self) {
        // TestState TLS Drop writes `end` when the thread-local is cleared.
        // Explicitly end here so the event is on disk before libtest reports.
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                if !st.ended {
                    st.ended = true;
                    let ok = !thread::panicking();
                    emit_end(&st.name, ok, st.start.elapsed().as_millis());
                }
            }
        });
    }
}

/// Narrative step (setup, kernel op, CAS inspect).
pub fn step(msg: impl AsRef<str>) {
    if !logging_enabled() {
        return;
    }
    ensure_started();
    emit(fields(
        "step",
        None,
        None,
        Some(msg.as_ref()),
        None,
        None,
        None,
    ));
}

/// Record an assert-style event. `ok` false still lets the caller panic.
pub fn record_assert(kind: &str, expr: &str, ok: bool, detail: &str) {
    if !logging_enabled() {
        return;
    }
    ensure_started();
    emit(fields(
        "assert",
        Some(ok),
        Some(kind),
        nonempty(detail),
        Some(expr),
        None,
        None,
    ));
}

/// Record `left == right` (debug-printed, truncated).
pub fn record_eq<L: Debug, R: Debug>(
    left_expr: &str,
    right_expr: &str,
    left: &L,
    right: &R,
    ok: bool,
    detail: &str,
) {
    if !logging_enabled() {
        return;
    }
    ensure_started();
    let pair = format!("{left_expr} == {right_expr}");
    emit(fields(
        "assert",
        Some(ok),
        Some("eq"),
        nonempty(detail),
        Some(&pair),
        Some(&trunc_debug(left)),
        Some(&trunc_debug(right)),
    ));
}

/// Record `left != right`.
pub fn record_ne<L: Debug, R: Debug>(
    left_expr: &str,
    right_expr: &str,
    left: &L,
    right: &R,
    ok: bool,
    detail: &str,
) {
    if !logging_enabled() {
        return;
    }
    ensure_started();
    let pair = format!("{left_expr} != {right_expr}");
    emit(fields(
        "assert",
        Some(ok),
        Some("ne"),
        nonempty(detail),
        Some(&pair),
        Some(&trunc_debug(left)),
        Some(&trunc_debug(right)),
    ));
}

/// Directory that holds `events.ndjson`.
pub fn review_dir() -> PathBuf {
    if let Ok(p) = std::env::var("ARKFS_TEST_REVIEW_DIR") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("target/arkfs-test-review")
}

fn logging_enabled() -> bool {
    if cfg!(any(miri, kani)) {
        return false;
    }
    !matches!(
        std::env::var("ARKFS_TEST_REVIEW"),
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false")
    )
}

fn ensure_started() {
    if !logging_enabled() {
        return;
    }
    INIT.call_once(|| {
        let dir = review_dir();
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("events.ndjson");
        if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
            *FILE.lock().unwrap() = Some(f);
            let _ = fs::write(
                dir.join("README.txt"),
                "ArkFS test review log (NDJSON).\n\
                 One JSON object per line in events.ndjson.\n\
                 Fields: ts_ms, test, event, ok, kind, expr, left, right, msg, elapsed_ms\n\
                 events: start | end | step | assert | panic\n\
                 Disable: ARKFS_TEST_REVIEW=0\n",
            );
        }
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let msg = info.to_string();
            emit(fields(
                "panic",
                Some(false),
                None,
                Some(&msg),
                None,
                None,
                None,
            ));
            prev(info);
        }));
    });
    STATE.with(|s| {
        if s.borrow().is_none() {
            let name = thread::current().name().unwrap_or("unknown").to_string();
            emit(fields("start", None, None, None, None, None, None));
            *s.borrow_mut() = Some(TestState {
                name,
                start: Instant::now(),
                ended: false,
            });
        }
    });
}

fn emit_end(_name: &str, ok: bool, elapsed_ms: u128) {
    if !logging_enabled() {
        return;
    }
    emit(format!(
        "{prefix}\"event\":\"end\",\"ok\":{ok},\"elapsed_ms\":{elapsed_ms}}}",
        prefix = common_prefix(),
        ok = if ok { "true" } else { "false" },
    ));
}

fn unix_ms() -> u128 {
    // Miri isolation rejects CLOCK_REALTIME (`clock_gettime`). Logging is
    // already disabled under Miri/Kani; this keeps accidental calls inert.
    if cfg!(any(miri, kani)) {
        return 0;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn common_prefix() -> String {
    let ts = unix_ms();
    let name = thread::current().name().unwrap_or("unknown").to_string();
    format!("{{\"ts_ms\":{ts},\"test\":\"{}\",", json_escape(&name))
}

fn fields(
    event: &str,
    ok: Option<bool>,
    kind: Option<&str>,
    msg: Option<&str>,
    expr: Option<&str>,
    left: Option<&str>,
    right: Option<&str>,
) -> String {
    let mut s = common_prefix();
    s.push_str("\"event\":\"");
    s.push_str(event);
    s.push('"');
    if let Some(ok) = ok {
        s.push_str(",\"ok\":");
        s.push_str(if ok { "true" } else { "false" });
    }
    if let Some(kind) = kind {
        s.push_str(",\"kind\":\"");
        s.push_str(&json_escape(kind));
        s.push('"');
    }
    if let Some(expr) = expr {
        s.push_str(",\"expr\":\"");
        s.push_str(&json_escape(expr));
        s.push('"');
    }
    if let Some(left) = left {
        s.push_str(",\"left\":\"");
        s.push_str(&json_escape(left));
        s.push('"');
    }
    if let Some(right) = right {
        s.push_str(",\"right\":\"");
        s.push_str(&json_escape(right));
        s.push('"');
    }
    if let Some(msg) = msg {
        s.push_str(",\"msg\":\"");
        s.push_str(&json_escape(msg));
        s.push('"');
    }
    s.push('}');
    s
}

fn emit(line: String) {
    if !logging_enabled() {
        return;
    }
    let mut g = FILE.lock().unwrap();
    if let Some(f) = g.as_mut() {
        let mut buf = line;
        buf.push('\n');
        let _ = f.write_all(buf.as_bytes());
        let _ = f.flush();
    }
}

fn trunc_debug<T: Debug>(v: &T) -> String {
    let s = format!("{v:?}");
    const MAX: usize = 800;
    if s.len() <= MAX {
        s
    } else {
        format!("{}…", &s[..MAX])
    }
}

fn nonempty(s: &str) -> Option<&str> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if c.is_control() => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// `assert!(cond)` that logs the predicate and result.
#[macro_export]
macro_rules! review_assert {
    ($cond:expr $(,)?) => {{
        let ok = $cond;
        $crate::record_assert("assert", stringify!($cond), ok, "");
        if !ok {
            ::std::panic!("assertion failed: {}", stringify!($cond));
        }
    }};
    ($cond:expr, $($arg:tt)+) => {{
        let ok = $cond;
        let msg = ::std::format!($($arg)+);
        $crate::record_assert("assert", stringify!($cond), ok, &msg);
        ::std::assert!(ok, "{}", msg);
    }};
}

/// `assert_eq!(left, right)` that logs both Debug values.
#[macro_export]
macro_rules! review_eq {
    ($left:expr, $right:expr $(,)?) => {{
        match (&$left, &$right) {
            (left_val, right_val) => {
                let ok = *left_val == *right_val;
                $crate::record_eq(
                    stringify!($left),
                    stringify!($right),
                    left_val,
                    right_val,
                    ok,
                    "",
                );
                if !ok {
                    ::std::assert_eq!(*left_val, *right_val);
                }
            }
        }
    }};
    ($left:expr, $right:expr, $($arg:tt)+) => {{
        match (&$left, &$right) {
            (left_val, right_val) => {
                let ok = *left_val == *right_val;
                let msg = ::std::format!($($arg)+);
                $crate::record_eq(
                    stringify!($left),
                    stringify!($right),
                    left_val,
                    right_val,
                    ok,
                    &msg,
                );
                if !ok {
                    ::std::assert_eq!(*left_val, *right_val, "{}", msg);
                }
            }
        }
    }};
}

/// `assert_ne!(left, right)` that logs both Debug values.
#[macro_export]
macro_rules! review_ne {
    ($left:expr, $right:expr $(,)?) => {{
        match (&$left, &$right) {
            (left_val, right_val) => {
                let ok = *left_val != *right_val;
                $crate::record_ne(
                    stringify!($left),
                    stringify!($right),
                    left_val,
                    right_val,
                    ok,
                    "",
                );
                if !ok {
                    ::std::assert_ne!(*left_val, *right_val);
                }
            }
        }
    }};
    ($left:expr, $right:expr, $($arg:tt)+) => {{
        match (&$left, &$right) {
            (left_val, right_val) => {
                let ok = *left_val != *right_val;
                let msg = ::std::format!($($arg)+);
                $crate::record_ne(
                    stringify!($left),
                    stringify!($right),
                    left_val,
                    right_val,
                    ok,
                    &msg,
                );
                if !ok {
                    ::std::assert_ne!(*left_val, *right_val, "{}", msg);
                }
            }
        }
    }};
}

/// Log `expr` then unwrap a `Result` / `Option` (panic is also logged).
#[macro_export]
macro_rules! review_ok {
    ($expr:expr) => {{
        $crate::step(stringify!($expr));
        match $expr {
            Ok(v) => {
                $crate::record_assert("ok", stringify!($expr), true, "");
                v
            }
            Err(e) => {
                $crate::record_assert("ok", stringify!($expr), false, &::std::format!("{e:?}"));
                ::std::panic!("review_ok! {} failed: {e:?}", stringify!($expr));
            }
        }
    }};
}

/// Unused helper so `review_dir` is not dead in the lib crate itself.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_assert as assert;

    /// Review dir is under target/ (or ARKFS_TEST_REVIEW_DIR).
    #[test]
    fn review_dir_ends_with_review() {
        let _g = guard();
        step("check review_dir");
        let p = review_dir();
        assert!(
            p.ends_with("arkfs-test-review") || std::env::var_os("ARKFS_TEST_REVIEW_DIR").is_some()
        );
    }
}
