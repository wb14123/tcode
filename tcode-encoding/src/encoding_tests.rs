//! Tests for the shared UTF-8 encoding helpers (`tcode-encoding`).
//!
//! Fixtures live under `target/test-tmp/tcode-encoding/<uuid>/` and are
//! removed by the `TestDir` RAII guard on both success and panic.
//!
//! The non-UTF-8 path entries are Unix-only (`std::os::unix::ffi::OsStrExt`).

use std::path::{Path, PathBuf};

use crate::{non_utf8_output_message, non_utf8_output_message_no_offset, path_to_str, split_lines};

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// Per-test temp dir under the workspace target dir; removed on drop
/// (cleanup runs on success and on panic).
struct TestDir(PathBuf);

impl TestDir {
    fn new(module: &str) -> Self {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../target/test-tmp/{module}"));
        std::fs::create_dir_all(&root).expect("failed to create test root");
        let dir = root.join(uuid::Uuid::new_v4().to_string());
        // Cleanup before: remove any stale leftover at this exact path.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ─── path_to_str tests (plan.md Section 6.2) ───────────────────────────────

#[test]
fn valid_utf8_path_ok() -> anyhow::Result<()> {
    let dir = TestDir::new("tcode-encoding");
    let file = dir.path().join("hello.txt");
    std::fs::write(&file, b"content")?;

    let s = path_to_str(&file)?;
    // Test-created paths are always UTF-8; `.to_str().unwrap()` is allowed.
    assert_eq!(s, file.to_str().unwrap());
    assert!(s.ends_with("hello.txt"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn non_utf8_path_errs_with_escaped_bytes() -> anyhow::Result<()> {
    let dir = TestDir::new("tcode-encoding");
    // Real entry with a non-UTF-8 name under the TestDir base.
    let non_utf8 = dir.path().join(OsStr::from_bytes(b"bad-\xff\xfe-name"));
    std::fs::write(&non_utf8, b"x")?;
    assert!(non_utf8.exists(), "fixture entry must exist");

    let err = path_to_str(&non_utf8).unwrap_err();
    // `Debug` for `Path`/`OsStr` on Unix emits `\xNN` byte escapes with
    // uppercase hex (e.g. `\xFF`); compare case-insensitively.
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("\\xff"),
        "error message should contain Debug-escaped bytes, got: {err}"
    );
    assert!(
        msg.contains("\\xfe"),
        "error message should contain Debug-escaped bytes, got: {err}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn debug_format_is_lossless() -> anyhow::Result<()> {
    let dir = TestDir::new("tcode-encoding");
    // Real entry with a non-UTF-8 name under the TestDir base.
    let non_utf8 = dir.path().join(OsStr::from_bytes(b"raw-\xff\xfe"));
    std::fs::write(&non_utf8, b"x")?;
    assert!(non_utf8.exists(), "fixture entry must exist");

    // The log-only display path (`{:?}`) escapes the raw bytes instead of
    // mangling them.
    let debug = format!("{non_utf8:?}").to_lowercase();
    assert!(
        debug.contains("\\xff"),
        "Debug format should contain escaped bytes, got: {non_utf8:?}"
    );
    assert!(
        debug.contains("\\xfe"),
        "Debug format should contain escaped bytes, got: {non_utf8:?}"
    );
    Ok(())
}

// ─── non_utf8_output_message tests ─────────────────────────────────────────

#[test]
fn non_utf8_output_message_exact_text() {
    // Build the invalid bytes at runtime so `invalid_from_utf8` does not
    // fire on a compile-time-invalid literal.
    let mut bytes: Vec<u8> = b"line".to_vec();
    bytes.push(0xff);
    let err = std::str::from_utf8(&bytes).unwrap_err();
    assert_eq!(err.valid_up_to(), 4);

    let msg = non_utf8_output_message("tool output", &err);
    assert_eq!(
        msg,
        "[Error: tool output is not valid UTF-8 and was omitted (first invalid byte at offset 4).\nThe raw bytes cannot be displayed as text. To inspect it, re-run the command with the output piped through base64, or ask the user to check the output's encoding.]"
    );
    // The message must not suggest a placeholder decode: it must never include
    // a replacement character (U+FFFD) and must point at an actionable
    // inspection path.
    assert!(!msg.contains('\u{FFFD}'));
    assert!(msg.contains("base64"));
}

#[test]
fn non_utf8_output_message_no_offset_exact_text() {
    let msg = non_utf8_output_message_no_offset("command output");
    assert_eq!(
        msg,
        "[Error: command output is not valid UTF-8 and was omitted.\nThe raw bytes cannot be displayed as text. To inspect it, re-run the command with the output piped through base64, or ask the user to check the output's encoding.]"
    );
    // Same invariants as the offset variant: never a placeholder decode, and
    // always an actionable inspection path.
    assert!(!msg.contains('\u{FFFD}'));
    assert!(msg.contains("base64"));
}

// ─── split_lines tests ─────────────────────────────────────────────────────

#[test]
fn split_lines_roundtrips_split_multibyte_char() {
    // `世` = E4 B8 96; feed `E4 B8` first (no complete line) then the rest.
    let mut buffer = Vec::new();
    let lines = split_lines(&mut buffer, b"data: \xe4\xb8");
    assert!(lines.is_empty(), "no `\n` yet, no complete line");
    assert_eq!(buffer, b"data: \xe4\xb8");

    let lines = split_lines(&mut buffer, b"\x96\n\n");
    assert_eq!(lines.len(), 2, "the completed line plus the empty line");
    assert_eq!(lines[0], b"data: \xe4\xb8\x96");
    // The completed line strict-decodes with zero replacement characters.
    let decoded = std::str::from_utf8(&lines[0]).expect("line must be valid UTF-8");
    assert_eq!(decoded, "data: 世");
    assert!(!decoded.contains('\u{FFFD}'));
    assert_eq!(lines[1], b"", "empty line between the two `\n`s");
    assert!(buffer.is_empty(), "all bytes consumed");
}

#[test]
fn split_lines_keeps_trailing_partial_line() {
    let mut buffer = Vec::new();
    let lines = split_lines(&mut buffer, b"a\nbcd");
    assert_eq!(lines, vec![b"a".to_vec()]);
    assert_eq!(
        buffer, b"bcd",
        "partial final line stays for the next chunk"
    );
}

#[test]
fn split_lines_invalid_byte_line_detectable() {
    let mut buffer = Vec::new();
    let lines = split_lines(&mut buffer, b"bad \xff line\n");
    assert_eq!(lines.len(), 1);
    // The caller strict-decodes the complete line and must be able to see the
    // invalid byte instead of silently receiving a placeholder.
    assert!(std::str::from_utf8(&lines[0]).is_err());
    assert!(buffer.is_empty());
}
