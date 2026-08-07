//! Shared strict-UTF-8 helpers for the tcode workspace.
//!
//! The tcode pipeline is UTF-8-only (JSON over HTTP). Non-UTF-8 bytes can
//! only enter the system from tools (file paths, file contents, command
//! output). Per the fail-loudly policy, these helpers convert strictly and
//! never silently replace invalid bytes with U+FFFD.

use std::path::Path;

/// Strictly converts a path to a UTF-8 string slice.
///
/// On failure returns an error whose message includes `{path:?}` (Rust Debug
/// escaping) so the raw bytes remain visible losslessly (e.g. `\xff`).
pub fn path_to_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("path is not valid UTF-8: {path:?}"))
}

/// Append `chunk` to a raw-byte line buffer and extract every complete
/// `\n`-terminated line (each without its trailing `\n`). Bytes after the
/// last `\n` (a partial final line) stay in `buffer` for the next chunk.
///
/// Splitting on `\n` (0x0A) is UTF-8-safe: 0x0A can never appear inside a
/// multi-byte UTF-8 sequence, so every extracted line contains complete
/// characters and can be strict-decoded by the caller.
pub fn split_lines(buffer: &mut Vec<u8>, chunk: &[u8]) -> Vec<Vec<u8>> {
    buffer.extend_from_slice(chunk);
    let mut lines = Vec::new();
    let mut line_start = 0;
    for (i, &byte) in buffer.iter().enumerate() {
        if byte == b'\n' {
            lines.push(buffer[line_start..i].to_vec());
            line_start = i + 1;
        }
    }
    buffer.drain(..line_start);
    lines
}

/// Builds the error-styled, actionable message for non-UTF-8 tool output.
///
/// `what` names the output being described (e.g. "tool output"); `err` is the
/// [`std::str::Utf8Error`] from a strict decode, whose
/// [`valid_up_to`](std::str::Utf8Error::valid_up_to) is the byte offset of
/// the first invalid byte.
pub fn non_utf8_output_message(what: &str, err: &std::str::Utf8Error) -> String {
    format!(
        "[Error: {what} is not valid UTF-8 and was omitted (first invalid byte at offset {}).\nThe raw bytes cannot be displayed as text. To inspect it, re-run the command with the output piped through base64, or ask the user to check the output's encoding.]",
        err.valid_up_to()
    )
}

/// Like [`non_utf8_output_message`] but without the byte-offset clause, for
/// callers that only have an `io::Error` (no `Utf8Error`), e.g. a streaming
/// line reader that reports "not valid UTF-8" without a position.
pub fn non_utf8_output_message_no_offset(what: &str) -> String {
    format!(
        "[Error: {what} is not valid UTF-8 and was omitted.\nThe raw bytes cannot be displayed as text. To inspect it, re-run the command with the output piped through base64, or ask the user to check the output's encoding.]"
    )
}

#[cfg(test)]
mod encoding_tests;
