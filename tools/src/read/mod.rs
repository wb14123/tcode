use std::path::Path;

use anyhow::{Result, anyhow};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

const DEFAULT_LIMIT: u64 = 500;
const DEFAULT_MAX_READ_CHARS: u64 = 20_000;

/// Binary file extensions that should be rejected without content inspection.
const BINARY_EXTENSIONS: &[&str] = &[
    // Executables / libraries
    "exe", "bin", "so", "dll", "o", "a", "dylib", "lib", "obj", "pyc", "pyo", "class",
    // Archives
    "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "zst", "lz4", // Images
    "jpg", "jpeg", "png", "gif", "bmp", "ico", "webp", "tiff", "tif", "psd", // Audio
    "mp3", "wav", "flac", "aac", "ogg", "wma", "m4a", // Video
    "mp4", "avi", "mkv", "mov", "wmv", "flv", "webm", // Fonts
    "woff", "woff2", "ttf", "eot", "otf", // Documents (binary)
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", // Database
    "db", "sqlite", "sqlite3", // Other
    "wasm",
];

/// Check if a file extension indicates a binary file.
fn is_binary_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let lower = ext.to_lowercase();
            BINARY_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

/// Check if file content appears to be binary by sampling bytes.
/// Returns true if more than 30% of bytes are non-printable ASCII control characters.
///
/// Only ASCII bytes (0x00-0x7F) are inspected. High bytes (0x80-0xFF) are skipped
/// because they appear in valid UTF-8 multibyte sequences (e.g. Chinese, Arabic, emoji).
/// Binary files are detected by their abundance of ASCII control characters (NUL, BEL, etc.)
/// which rarely appear in real text of any language.
fn is_binary_content(data: &[u8]) -> bool {
    if data.is_empty() {
        return false;
    }
    let non_printable = data
        .iter()
        .filter(|&&b| {
            // Skip non-ASCII bytes — they're likely UTF-8 multibyte (valid text)
            b.is_ascii()
                // Among ASCII bytes, flag control chars (0x00-0x1F, 0x7F)
                // except common whitespace (tab, newline, carriage return)
                && !matches!(b, b'\t' | b'\n' | b'\r' | 0x20..=0x7E)
        })
        .count();
    (non_printable as f64 / data.len() as f64) > 0.3
}

/// Result of processing a single line for output.
struct ProcessedLine {
    /// The content line: "N| verbatim content"
    content_line: String,
    /// Optional annotation lines after the content line (e.g. truncation / offset info)
    annotations: Vec<String>,
    /// Number of content characters consumed (for char cap tracking)
    chars_used: usize,
    /// The character end position (chars_start + chars_used), for truncation annotations
    chars_end: u64,
    /// True when the global char cap was exhausted — signals the caller to stop reading.
    hit_char_cap: bool,
}

/// Result when a line is skipped — e.g. first_line_offset beyond line length.
const SKIPPED_LINE: ProcessedLine = ProcessedLine {
    content_line: String::new(),
    annotations: Vec::new(),
    chars_used: 0,
    chars_end: 0,
    hit_char_cap: false,
};

/// Process a raw line into plain-text output, handling first_line_offset and char cap truncation.
///
/// `line_buf_capped` indicates the line was too long to fully read into the buffer,
/// so the content is incomplete and a truncation annotation will be emitted.
fn process_line(
    line_content: &str,
    line_num: u64,
    effective_offset: usize,
    chars_remaining: usize,
    line_buf_capped: bool,
) -> ProcessedLine {
    if chars_remaining == 0 {
        return ProcessedLine {
            content_line: String::new(),
            annotations: Vec::new(),
            chars_used: 0,
            chars_end: 0,
            hit_char_cap: true,
        };
    }

    // Step 1: Apply effective_offset to get the visible portion
    let (content, chars_start) = if effective_offset > 0 {
        let byte_offset = line_content
            .char_indices()
            .nth(effective_offset)
            .map(|(i, _)| i)
            .unwrap_or(line_content.len());
        // Offset beyond line length → skip this line entirely
        if byte_offset >= line_content.len() {
            return SKIPPED_LINE;
        }
        (&line_content[byte_offset..], effective_offset as u64)
    } else {
        (line_content, 0u64)
    };

    let content_chars = content.chars().count();

    // Step 2: Apply char cap — truncate if content exceeds remaining budget
    let (final_content, hit_cap) = if content_chars > chars_remaining {
        let byte_end = content
            .char_indices()
            .nth(chars_remaining)
            .map(|(i, _)| i)
            .unwrap_or(content.len());
        (&content[..byte_end], true)
    } else {
        (content, false)
    };

    let chars_used = if hit_cap {
        chars_remaining
    } else {
        content_chars
    };
    let chars_end = chars_start + chars_used as u64;
    let is_truncated = line_buf_capped || hit_cap;

    // Build content line: "N| verbatim content"
    let content_line = format!("{}| {}", line_num, final_content);

    // Build annotation lines
    let mut annotations = Vec::new();
    if chars_start > 0 || is_truncated {
        if chars_start > 0 && is_truncated {
            annotations.push(format!(
                "#| Line {} above starts at character {} and is truncated at character {}.",
                line_num, chars_start, chars_end
            ));
        } else if chars_start > 0 {
            annotations.push(format!(
                "#| Line {} above starts at character {}.",
                line_num, chars_start
            ));
        } else {
            annotations.push(format!(
                "#| Line {} above is truncated at character {}.",
                line_num, chars_end
            ));
        }
        // Per-line "To continue" only for buffer-cap truncation (same line).
        // Global char cap truncation gets the footer-level "To read more" instead,
        // which includes the line number change.
        if line_buf_capped {
            annotations.push(format!(
                "#| To continue, re-read with first_line_offset={}.",
                chars_end
            ));
        }
    }

    ProcessedLine {
        content_line,
        annotations,
        chars_used,
        chars_end,
        hit_char_cap: hit_cap,
    }
}

/// Try to emit a processed line into `batch`.
/// Returns `Some(chars_used)` if a line was appended, `None` if the char cap
/// prevented any output. `*was_truncated` is set when the global char cap is hit.
/// `*last_truncated_line` and `*last_truncated_offset` are set when mid-line truncation occurs.
#[allow(clippy::too_many_arguments)]
fn emit_line(
    line_content: &str,
    line_num: u64,
    flo_applied: bool,
    flo: usize,
    chars_consumed: usize,
    max_chars: usize,
    line_buf_capped: bool,
    batch: &mut String,
    was_truncated: &mut bool,
    last_truncated_line: &mut u64,
    last_truncated_offset: &mut u64,
) -> Option<usize> {
    let remaining = max_chars.saturating_sub(chars_consumed);
    let effective_offset = if flo_applied { 0 } else { flo };
    let processed = process_line(
        line_content,
        line_num,
        effective_offset,
        remaining,
        line_buf_capped,
    );
    if processed.hit_char_cap && processed.content_line.is_empty() {
        *was_truncated = true;
        *last_truncated_line = line_num;
        *last_truncated_offset = 0;
        return None;
    }
    // Line was skipped (e.g. first_line_offset beyond line length) — nothing to emit
    if processed.content_line.is_empty() {
        return None;
    }
    batch.push('\n');
    batch.push_str(&processed.content_line);
    for annotation in &processed.annotations {
        batch.push('\n');
        batch.push_str(annotation);
    }
    if processed.hit_char_cap {
        *was_truncated = true;
        *last_truncated_line = line_num;
        *last_truncated_offset = processed.chars_end;
    }
    Some(processed.chars_used)
}

/// Read a directory and return a plain-text listing with a footer.
async fn read_directory(path: &Path) -> Result<String> {
    let mut dir_iter = tokio::fs::read_dir(path)
        .await
        .map_err(|e| anyhow!("Failed to read directory {}: {}", path.display(), e))?;

    let mut items: Vec<tokio::fs::DirEntry> = Vec::new();
    while let Some(entry) = dir_iter.next_entry().await.map_err(|e| {
        anyhow!(
            "Failed to read directory entry in {}: {}",
            path.display(),
            e
        )
    })? {
        items.push(entry);
    }
    items.sort_by_key(|e| e.file_name());

    let mut entries: Vec<String> = Vec::new();
    for entry in items {
        let name = entry.file_name().to_string_lossy().to_string();
        match entry.file_type().await {
            Ok(ft) if ft.is_dir() => entries.push(format!("{}/", name)),
            _ => entries.push(name),
        }
    }

    let count = entries.len();
    let path_display = path.to_string_lossy().into_owned();
    let mut output = entries.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(&format!(
        "#| Directory: {} ({} entries)",
        path_display, count
    ));
    Ok(output)
}

/// Read a file or directory from the local filesystem. Error if path doesn't exist.
///
/// - file_path must be absolute. Returns up to 500 lines from start by default.
/// - offset is the 1-indexed line number to start from.
/// - For files over ~200 lines, use `grep` first to locate lines, then read with offset/limit.
/// - For small portions, use smaller `max_read_chars` (5000-10000) or targeted offset+limit.
/// - Use glob tool if unsure of path. Call in parallel for multiple files.
/// - Avoid tiny repeated slices (30 line chunks) — read a larger window instead.
///
/// Output format: content lines start with `N| ` (line number, pipe, space); everything after is verbatim content.
/// Annotation lines start with `#|` and describe truncation/offset status.
/// NOTE: `#|` annotations are metadata, NOT file content. A line like `42| #| something` means line 42 contains
/// the text `#| something` — the `42| ` prefix is the sole disambiguator.
///
///   #| File: /absolute/path                  (header — first line of every file output)
///   #| Line N above is truncated at character X.
///   #| Line N above starts at character X.
///   #| Line N above starts at character X and is truncated at character Y.
///   #| To continue, re-read with first_line_offset=X.
///   #| Lines X-Y of Z total.                 (footer — last line of every file output)
///   #| No content after line Z.              (offset past EOF)
///   #| Output capped at N characters (max_read_chars=N, file size: M bytes).
///   #| To read more, re-read with offset=X and first_line_offset=Y.
///   #| Directory: /path (N entries)          (footer — last line of directory output)
///
/// All character counts are Unicode scalar values (Rust `char`), not byte offsets.
///
/// Char cap: 20000 by default (max_read_chars adjustable up to 50000).
/// Pagination: offset/limit for lines; first_line_offset to skip chars within the first line.
#[tool]
pub fn read(
    ctx: ToolContext,
    /// Absolute path to the file or directory to read
    file_path: String,
    /// Line number to start reading from (1-indexed, default: 1)
    #[serde(default)]
    offset: Option<u64>,
    /// Maximum number of lines to read (default: 2000)
    #[serde(default)]
    limit: Option<u64>,
    /// Maximum characters to read from the file (default: 50000)
    #[serde(default)]
    max_read_chars: Option<u64>,
    /// Character offset within the first line to start reading from (default: 0).
    /// Useful for reading very long lines across multiple passes.
    #[serde(default)]
    first_line_offset: Option<u64>,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let path = Path::new(&file_path);

        if !path.is_absolute() {
            yield Err(anyhow!("file_path must be an absolute path, got: {}", file_path));
            return;
        }

        let metadata = match tokio::fs::metadata(path).await {
            Ok(m) => m,
            Err(_) => {
                yield Err(anyhow!("Path does not exist: {}", file_path));
                return;
            }
        };

        // Permission check for paths outside current working directory
        if let Err(e) = crate::file_permission::check_file_read_permission(
            &ctx.permission, path, metadata.is_dir(),
        ).await {
            yield Err(e);
            return;
        }

        // Handle directory
        if metadata.is_dir() {
            match read_directory(path).await {
                Ok(output) => yield Ok(output),
                Err(e) => yield Err(e),
            }
            return;
        }

        // Binary extension check (no I/O needed)
        if is_binary_extension(path) {
            yield Err(anyhow!(
                "Cannot read binary file: {}. This appears to be a binary file based on its extension.",
                path.display()
            ));
            return;
        }

        // Open file
        let file = match File::open(path).await {
            Ok(f) => f,
            Err(e) => {
                yield Err(anyhow!("Failed to open file {}: {}", path.display(), e));
                return;
            }
        };
        let file_size = match file.metadata().await {
            Ok(m) => m.len(),
            Err(e) => {
                yield Err(anyhow!("Failed to get file metadata {}: {}", path.display(), e));
                return;
            }
        };

        // Binary content check via fill_buf (peeks without consuming)
        let mut reader = BufReader::new(file);
        {
            let peek = match reader.fill_buf().await {
                Ok(b) => b,
                Err(e) => {
                    yield Err(anyhow!("Failed to read file {}: {}", path.display(), e));
                    return;
                }
            };
            if is_binary_content(peek) {
                yield Err(anyhow!(
                    "Cannot read binary file: {}. The file contains too many non-printable characters.",
                    path.display()
                ));
                return;
            }
        }

        let start = offset.unwrap_or(1).max(1);
        let lim = limit.unwrap_or(DEFAULT_LIMIT);
        let max_chars = max_read_chars.unwrap_or(DEFAULT_MAX_READ_CHARS).max(1) as usize;
        let flo = first_line_offset.unwrap_or(0) as usize;

        let path_display = path.to_string_lossy().into_owned();

        // Yield header
        yield Ok(format!("#| File: {}", path_display));

        let mut current_line: u64 = 0;
        let mut lines_yielded: u64 = 0;
        let mut flo_applied = false;
        let mut chars_consumed: usize = 0;
        let mut was_truncated = false;
        let mut partial_line: Vec<u8> = Vec::new();
        // Char count of partial_line buffer — capped at max_chars to bound memory usage
        // on extremely long lines (e.g. minified JS). This is NOT the global output char cap.
        let mut partial_buf_chars: usize = 0;
        let mut partial_capped = false;
        let mut batch = String::new();
        // Track the last truncated line info for the final "To read more" annotation
        let mut last_truncated_line: u64 = 0;
        let mut last_truncated_offset: u64 = 0;
        let mut first_emitted_line: u64 = 0;

        loop {
            let buf = match reader.fill_buf().await {
                Ok(b) => b,
                Err(e) => {
                    yield Err(anyhow!("Failed to read file {}: {}", path_display, e));
                    return;
                }
            };
            if buf.is_empty() {
                // EOF — emit final partial line if any
                if !partial_line.is_empty() && lines_yielded < lim && !was_truncated {
                    current_line += 1;
                    if current_line >= start {
                        let line_str = String::from_utf8_lossy(&partial_line);
                        if emit_line(&line_str, current_line, flo_applied, flo, chars_consumed, max_chars, partial_capped, &mut batch, &mut was_truncated, &mut last_truncated_line, &mut last_truncated_offset).is_some() {
                            if first_emitted_line == 0 {
                                first_emitted_line = current_line;
                            }
                            lines_yielded += 1;
                        }
                    }
                }
                break;
            }

            let buf_len = buf.len();
            // Copy before consuming — consume invalidates the borrow
            let buf_owned = buf.to_vec();
            reader.consume(buf_len);

            let mut pos = 0;
            while pos < buf_len {
                match buf_owned[pos..].iter().position(|&b| b == b'\n') {
                    Some(nl_offset) => {
                        let line_end = pos + nl_offset;
                        current_line += 1;

                        if lines_yielded < lim && !was_truncated && current_line >= start {
                            if !partial_capped {
                                partial_line.extend_from_slice(&buf_owned[pos..line_end]);
                            }
                            let line_str = String::from_utf8_lossy(&partial_line);
                            if let Some(chars_used) = emit_line(&line_str, current_line, flo_applied, flo, chars_consumed, max_chars, partial_capped, &mut batch, &mut was_truncated, &mut last_truncated_line, &mut last_truncated_offset) {
                                chars_consumed += chars_used + 1; // +1 for \n
                                flo_applied = true;
                                if first_emitted_line == 0 {
                                    first_emitted_line = current_line;
                                }
                                lines_yielded += 1;
                            } else if !flo_applied {
                                // Line was skipped (e.g. offset beyond its length).
                                // The first_line_offset was applied to this line — don't
                                // re-apply it to subsequent lines.
                                flo_applied = true;
                            }
                        }
                        partial_line.clear();
                        partial_buf_chars = 0;
                        partial_capped = false;
                        pos = line_end + 1;
                    }
                    None => {
                        // No newline — accumulate into partial_line buffer.
                        // Cap at max_chars characters to bound memory on very long lines.
                        if !partial_capped {
                            let remaining = &buf_owned[pos..buf_len];
                            let char_space = max_chars.saturating_sub(partial_buf_chars);
                            if char_space == 0 {
                                partial_capped = true;
                            } else {
                                let remaining_str = String::from_utf8_lossy(remaining);
                                let take_chars = remaining_str.chars().count().min(char_space);
                                let byte_end = remaining_str
                                    .char_indices()
                                    .nth(take_chars)
                                    .map(|(i, _)| i)
                                    .unwrap_or(remaining.len());
                                partial_line.extend_from_slice(&remaining[..byte_end]);
                                partial_buf_chars += take_chars;
                                if partial_buf_chars >= max_chars {
                                    partial_capped = true;
                                }
                            }
                        }
                        break;
                    }
                }
            }

            if !batch.is_empty() {
                yield Ok(std::mem::take(&mut batch));
            }

            if lines_yielded >= lim || was_truncated {
                // Fast-count remaining lines for total_lines metadata
                let mut count_buf = [0u8; 8192];
                // Tracks whether the file ends with a newline; initialized to 0
                // so we don't falsely count an extra line if the loop body never runs.
                let mut last_byte = 0u8;
                loop {
                    match reader.read(&mut count_buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            current_line += count_buf[..n].iter().filter(|&&b| b == b'\n').count() as u64;
                            last_byte = count_buf[n - 1];
                        }
                        Err(e) => {
                            tracing::warn!("Error counting remaining lines in {}: {}", path_display, e);
                            break;
                        }
                    }
                }
                if !partial_line.is_empty() || (last_byte != 0 && last_byte != b'\n') {
                    current_line += 1;
                }
                break;
            }
        }

        if !batch.is_empty() {
            yield Ok(std::mem::take(&mut batch));
        }

        // Build footer
        let actual_end = if lines_yielded > 0 {
            first_emitted_line + lines_yielded - 1
        } else {
            0
        };
        let total_lines = current_line;
        let mut closing = String::new();

        // Offset beyond EOF or empty file
        if lines_yielded == 0 {
            if total_lines == 0 {
                closing.push_str("\n#| File is empty.");
            } else {
                closing.push_str(&format!("\n#| No content after line {}.", start));
            }
        }

        closing.push_str(&format!(
            "\n#| Lines {}-{} of {} total.",
            if lines_yielded > 0 { first_emitted_line } else { 0 },
            actual_end,
            total_lines
        ));

        if was_truncated {
            closing.push_str(&format!(
                "\n#| Output capped at {} characters (max_read_chars={}, file size: {} bytes).",
                max_chars, max_chars, file_size
            ));
            if last_truncated_line > 0 {
                closing.push_str(&format!(
                    "\n#| To read more, re-read with offset={} and first_line_offset={}.",
                    last_truncated_line, last_truncated_offset
                ));
            }
        }

        yield Ok(closing);
    }
}

#[cfg(test)]
mod read_tests;
