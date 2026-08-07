//! Tests for the SSE byte-buffer line extraction (shared `tcode_encoding::split_lines`).

use tcode_encoding::split_lines;

/// A multi-byte UTF-8 character (`世` = E4 B8 96) split across two chunks must
/// round-trip intact: regression for GitHub issue #2 (Chinese text garbled
/// while streaming, caused by decoding each network chunk independently).
#[test]
fn split_multibyte_char_across_chunks_roundtrips() {
    let mut buffer = Vec::new();
    assert!(split_lines(&mut buffer, b"data: \xe4\xb8").is_empty());
    assert_eq!(buffer, b"data: \xe4\xb8".to_vec());

    let lines = split_lines(&mut buffer, b"\x96\n\n");
    assert_eq!(lines.len(), 2); // `data: 世` plus the empty boundary line
    assert!(buffer.is_empty());

    let decoded = std::str::from_utf8(&lines[0]).expect("complete line must be valid UTF-8");
    assert_eq!(decoded, "data: 世");
    assert!(!decoded.contains('\u{FFFD}'));

    assert!(lines[1].is_empty());
}

/// A complete line containing an invalid byte fails strict decoding; the error
/// reports the byte offset of the first invalid byte.
#[test]
fn invalid_utf8_line_fails_strict_decode() {
    let mut buffer = Vec::new();
    let lines = split_lines(&mut buffer, b"data: a\xffb\n");
    assert_eq!(lines.len(), 1);

    let err = std::str::from_utf8(&lines[0]).expect_err("invalid UTF-8 must fail strict decode");
    // `\xff` sits at byte offset 7 within `b"data: a\xffb"`.
    assert_eq!(err.valid_up_to(), 7);
}

/// A CRLF line ending leaves the trailing `\r` on the line bytes; the stream
/// trims it with `trim_ascii_end` before decoding so it never reaches the
/// JSON parser.
#[test]
fn crlf_trailing_cr_is_trimmed_before_decode() {
    let mut buffer = Vec::new();
    let lines = split_lines(&mut buffer, b"data: hello\r\n");
    assert_eq!(lines.len(), 1);

    let trimmed = lines[0].trim_ascii_end();
    let decoded = std::str::from_utf8(trimmed).expect("valid UTF-8");
    assert_eq!(decoded, "data: hello");
}
