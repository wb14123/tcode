#[cfg(test)]
mod grep_tool_tests;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Result, anyhow};
use grep::regex::RegexMatcherBuilder;
use grep::searcher::sinks::UTF8;
use grep::searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

const MAX_RESULTS: usize = 100;
const MAX_LINE_LEN: usize = 2000;

struct MatchLine {
    line_number: u64,
    content: String,
}

struct FileMatches {
    path: PathBuf,
    mtime: SystemTime,
    lines: Vec<MatchLine>,
}

/// Perform content search across files in `search_dir`.
/// Synchronous — intended to run inside `spawn_blocking`.
fn search_grep(
    search_dir: &Path,
    pattern: &str,
    include: Option<&str>,
) -> Result<(Vec<FileMatches>, usize)> {
    let matcher = RegexMatcherBuilder::new()
        .build(pattern)
        .map_err(|e| anyhow!("Invalid regex pattern '{}': {}", pattern, e))?;

    let mut walk_builder = WalkBuilder::new(search_dir);
    walk_builder.hidden(false); // false = DO search hidden files

    if let Some(include_pattern) = include {
        let mut overrides = OverrideBuilder::new(search_dir);
        overrides
            .add(include_pattern)
            .map_err(|e| anyhow!("Invalid include pattern '{}': {}", include_pattern, e))?;
        let overrides = overrides
            .build()
            .map_err(|e| anyhow!("Failed to build include filter: {}", e))?;
        walk_builder.overrides(overrides);
    }

    let walker = walk_builder.build();

    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0x00))
        .build();

    let mut all_file_matches: Vec<FileMatches> = Vec::new();
    let mut total_match_count: usize = 0;

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("grep walk error: {}", e);
                continue;
            }
        };

        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let file_path = entry.into_path();

        let mtime = match std::fs::metadata(&file_path) {
            Ok(m) => match m.modified() {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("failed to get mtime for {}: {}", file_path.display(), e);
                    SystemTime::UNIX_EPOCH
                }
            },
            Err(e) => {
                tracing::warn!("failed to get metadata for {}: {}", file_path.display(), e);
                continue;
            }
        };

        let mut file_lines: Vec<MatchLine> = Vec::new();

        let search_result = searcher.search_path(
            &matcher,
            &file_path,
            UTF8(|line_number, line_content| {
                let content = line_content.trim_end();
                let content = if content.chars().count() > MAX_LINE_LEN {
                    content.chars().take(MAX_LINE_LEN).collect::<String>()
                } else {
                    content.to_string()
                };
                file_lines.push(MatchLine {
                    line_number,
                    content,
                });
                Ok(true)
            }),
        );

        match search_result {
            Ok(()) => {}
            Err(e) => {
                tracing::debug!("grep search error for {}: {}", file_path.display(), e);
                continue;
            }
        }

        if !file_lines.is_empty() {
            total_match_count += file_lines.len();
            all_file_matches.push(FileMatches {
                path: file_path,
                mtime,
                lines: file_lines,
            });
        }
    }

    // Sort by mtime descending (most recently modified first)
    all_file_matches.sort_by(|a, b| b.mtime.cmp(&a.mtime));

    Ok((all_file_matches, total_match_count))
}

/// Fast content search tool that works with any codebase size.
/// Searches file contents using regular expressions.
/// Supports full regex syntax (eg. "log.*Error", "function\s+\w+", etc.)
/// Filter files by pattern with the include parameter (eg. "*.js", "*.{ts,tsx}")
/// Returns file paths and line numbers with at least one match sorted by modification time.
/// Results are capped at 100 matches. If truncated, refine your pattern or use the include
/// parameter to narrow the search. Individual matching lines are truncated at 2000 characters.
/// Use this tool when you need to find files containing specific patterns.
/// If you need to identify/count the number of matches within files, use the Bash tool
/// with `rg` (ripgrep) directly. Do NOT use `grep`.
/// When you are doing an open-ended search that may require multiple rounds of globbing
/// and grepping, delegate to a subagent instead.
#[tool]
pub fn grep(
    ctx: ToolContext,
    /// Regex pattern to search for in file contents
    pattern: String,
    /// Directory to search in (defaults to current working directory)
    #[serde(default)]
    path: Option<String>,
    /// File glob filter (e.g. "*.js", "*.{ts,tsx}")
    #[serde(default)]
    include: Option<String>,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let cwd = match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                yield Err(anyhow!("Failed to get current directory: {}", e));
                return;
            }
        };

        let search_dir = match path {
            Some(p) => PathBuf::from(p),
            None => cwd,
        };

        match tokio::fs::metadata(&search_dir).await {
            Ok(m) if m.is_dir() => {}
            Ok(_) => {
                yield Err(anyhow!("Search path is not a directory: {}", search_dir.display()));
                return;
            }
            Err(e) => {
                yield Err(anyhow!("Failed to access search path {}: {}", search_dir.display(), e));
                return;
            }
        }

        if let Err(e) = crate::file_permission::check_file_read_permission(
            &ctx.permission, &search_dir, true,
        ).await {
            yield Err(e);
            return;
        }

        let search_result = tokio::task::spawn_blocking(move || {
            search_grep(
                &search_dir,
                &pattern,
                include.as_deref(),
            )
        }).await;

        let (file_matches, total_match_count) = match search_result {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                yield Err(e);
                return;
            }
            Err(e) => {
                yield Err(anyhow!("Grep search task failed: {}", e));
                return;
            }
        };

        if file_matches.is_empty() {
            yield Ok("No files found".to_string());
            return;
        }

        let truncated = total_match_count > MAX_RESULTS;

        let mut output = String::new();
        if truncated {
            output.push_str(&format!(
                "Found {} matches (showing first {})\n",
                total_match_count, MAX_RESULTS
            ));
        } else {
            output.push_str(&format!("Found {} matches\n", total_match_count));
        }

        let mut matches_emitted = 0;
        for file_match in &file_matches {
            if matches_emitted >= MAX_RESULTS {
                break;
            }

            let file_path_str = file_match.path.to_string_lossy();
            output.push_str(&format!("\n{}:\n", file_path_str));

            for line in &file_match.lines {
                if matches_emitted >= MAX_RESULTS {
                    break;
                }
                output.push_str(&format!("  Line {}: {}\n", line.line_number, line.content));
                matches_emitted += 1;
            }
        }

        if truncated {
            output.push_str(&format!(
                "\n(Results truncated: showing {} of {} total matches)",
                MAX_RESULTS, total_match_count
            ));
        }

        yield Ok(output);
    }
}
