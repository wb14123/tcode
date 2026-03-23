use std::path::Path;

use anyhow::{Result, anyhow};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;
use quick_xml::escape::escape;
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
    xml: String,
    chars_used: usize,
    /// True when the global char cap was exhausted — signals the caller to stop reading.
    hit_char_cap: bool,
}

/// Process a raw line into XML output, handling first_line_offset and char cap truncation.
///
/// `line_buf_capped` indicates the line was too long to fully read into the buffer,
/// so the content is incomplete and the XML tag will include `truncated="true"`.
fn process_line(
    line_content: &str,
    line_num: u64,
    is_first: bool,
    first_line_offset: usize,
    chars_remaining: usize,
    line_buf_capped: bool,
) -> ProcessedLine {
    if chars_remaining == 0 {
        return ProcessedLine {
            xml: String::new(),
            chars_used: 0,
            hit_char_cap: true,
        };
    }

    // Step 1: Apply first_line_offset to get the visible portion
    let effective_offset = if is_first { first_line_offset } else { 0 };
    let (content, chars_start) = if effective_offset > 0 {
        let byte_offset = line_content
            .char_indices()
            .nth(effective_offset)
            .map(|(i, _)| i)
            .unwrap_or(line_content.len());
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
    let xml_truncated = line_buf_capped || hit_cap;

    // Only add char range attrs when there's an offset or truncation to report
    let char_info = if chars_start > 0 || xml_truncated {
        Some((chars_start, chars_start + chars_used as u64, xml_truncated))
    } else {
        None
    };

    ProcessedLine {
        xml: format_line_xml(line_num, final_content, char_info),
        chars_used,
        hit_char_cap: hit_cap,
    }
}

/// Format a single line as XML.
///
/// `char_info` is `(chars_start, chars_end, truncated)` — only present when the line
/// was read from an offset or truncated. Plain lines get just `<line n="...">`.
fn format_line_xml(line_num: u64, content: &str, char_info: Option<(u64, u64, bool)>) -> String {
    let escaped = escape(content);
    let mut tag = format!("<line n=\"{line_num}\"");
    if let Some((start, end, truncated)) = char_info {
        tag.push_str(&format!(" chars_start=\"{start}\" chars_end=\"{end}\""));
        if truncated {
            tag.push_str(" truncated=\"true\"");
        }
    }
    tag.push('>');
    tag.push_str(&escaped);
    tag.push_str("</line>");
    tag
}

/// Try to emit a processed line into `batch`.
/// Returns `Some(chars_used)` if a line was appended, `None` if the char cap
/// prevented any output. `*was_truncated` is set in either truncation case.
#[allow(clippy::too_many_arguments)]
fn emit_line(
    line_content: &str,
    line_num: u64,
    lines_yielded: u64,
    flo: usize,
    chars_consumed: usize,
    max_chars: usize,
    line_buf_capped: bool,
    batch: &mut String,
    was_truncated: &mut bool,
) -> Option<usize> {
    let remaining = max_chars.saturating_sub(chars_consumed);
    let is_first = lines_yielded == 0;
    let line_flo = if is_first { flo } else { 0 };
    let processed = process_line(
        line_content,
        line_num,
        is_first,
        line_flo,
        remaining,
        line_buf_capped,
    );
    if processed.hit_char_cap && processed.xml.is_empty() {
        *was_truncated = true;
        return None;
    }
    batch.push('\n');
    batch.push_str(&processed.xml);
    if processed.hit_char_cap {
        *was_truncated = true;
    }
    Some(processed.chars_used)
}

/// Read a directory and return XML-formatted listing.
async fn read_directory(path: &Path) -> Result<String> {
    let mut entries: Vec<String> = Vec::new();

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

    for entry in items {
        let name = entry.file_name().to_string_lossy().to_string();
        match entry.file_type().await {
            Ok(ft) if ft.is_dir() => entries.push(format!("{}/", escape(&name))),
            _ => entries.push(escape(&name).into_owned()),
        }
    }

    let path_display = path.to_string_lossy().into_owned();
    let path_str = escape(&path_display);
    Ok(format!(
        "<directory path=\"{}\">\n<entries>\n{}\n</entries>\n</directory>",
        path_str,
        entries.join("\n")
    ))
}

/// Read a file or directory from the local filesystem. If the path does not exist, an error is returned.
///
/// Usage:
/// - The file_path parameter should be an absolute path.
/// - By default, this tool returns up to 500 lines from the start of the file.
/// - The offset parameter is the line number to start from (1-indexed).
/// - To read later sections, call this tool again with a larger offset.
/// - For files over ~200 lines, prefer using `grep` first to locate the relevant lines,
///   then read only those lines with `offset` and `limit` to avoid consuming unnecessary context.
/// - When you only need a small portion of a file (e.g. checking a function signature),
///   use a smaller `max_read_chars` (e.g. 5000-10000) or a targeted `offset`+`limit`.
/// - If you are unsure of the correct file path, use the glob tool to look up filenames by glob pattern.
/// - Call this tool in parallel when you know there are multiple files you want to read.
/// - Avoid tiny repeated slices (30 line chunks). If you need more context, read a larger window.
///
/// Output format:
/// - Files are returned as XML with line numbers streamed incrementally.
/// - Each line is wrapped in a `<line n="...">` tag with its 1-indexed line number.
/// - When a line is read from an offset or truncated, the `<line>` tag includes `chars_start` and `chars_end` attributes showing the character range returned. If the line was truncated (by char cap or because it was too long to fully read), the tag also includes `truncated="true"`.
/// - Directories are returned as: `<directory path="..."><entries>name\nsubdir/\n...</entries></directory>`
///   Subdirectories have a trailing "/".
///
/// Character cap:
/// - By default, at most 20000 characters are read from the file. Use max_read_chars to adjust (up to 50000).
/// - If the character cap is reached mid-line, the last line is truncated. Its `<line>` tag will include `chars_start`, `chars_end`, and `truncated="true"` showing exactly which portion of the line was returned.
///
/// Pagination:
/// - Use offset and limit for line-based pagination.
/// - Use first_line_offset to skip characters within the first returned line (for reading very long lines across multiple passes).
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
        let max_chars = max_read_chars.unwrap_or(DEFAULT_MAX_READ_CHARS) as usize;
        let flo = first_line_offset.unwrap_or(0) as usize;

        // Yield opening XML tag (total_lines not known yet — reported at end)
        let path_display = path.to_string_lossy().into_owned();
        yield Ok(format!(
            "<file path=\"{}\">\n<lines start=\"{}\">",
            escape(&path_display), start
        ));

        let mut current_line: u64 = 0;
        let mut lines_yielded: u64 = 0;
        let mut chars_consumed: usize = 0;
        let mut was_truncated = false;
        let mut partial_line: Vec<u8> = Vec::new();
        // Char count of partial_line buffer — capped at max_chars to bound memory usage
        // on extremely long lines (e.g. minified JS). This is NOT the global output char cap.
        let mut partial_buf_chars: usize = 0;
        let mut partial_capped = false;
        let mut batch = String::new();

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
                        if emit_line(&line_str, current_line, lines_yielded, flo, chars_consumed, max_chars, partial_capped, &mut batch, &mut was_truncated).is_some() {
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
                            if let Some(chars_used) = emit_line(&line_str, current_line, lines_yielded, flo, chars_consumed, max_chars, partial_capped, &mut batch, &mut was_truncated) {
                                chars_consumed += chars_used + 1; // +1 for \n
                                lines_yielded += 1;
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

        // Closing tags with end and total
        let actual_end = if lines_yielded > 0 {
            start + lines_yielded - 1
        } else {
            0
        };
        let total_lines = current_line;
        let mut closing = format!(
            "\n</lines>\n<read end=\"{}\" total_lines=\"{}\" />\n</file>",
            actual_end, total_lines
        );
        if was_truncated {
            closing.push_str(&format!(
                "\n(File truncated at {} chars, total file size: {} bytes)",
                max_chars, file_size
            ));
        }
        yield Ok(closing);
    }
}
