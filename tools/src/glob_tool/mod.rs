use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Result, anyhow};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

const MAX_RESULTS: usize = 100;

/// Perform the directory walk and collect matching files.
/// This is all synchronous I/O, intended to run inside `spawn_blocking`.
fn walk_glob(search_dir: &Path, pattern: &str) -> Result<Vec<(PathBuf, SystemTime)>> {
    let mut overrides = OverrideBuilder::new(search_dir);
    overrides
        .add(pattern)
        .map_err(|e| anyhow!("Invalid glob pattern '{}': {}", pattern, e))?;
    let overrides = overrides
        .build()
        .map_err(|e| anyhow!("Failed to build glob matcher: {}", e))?;

    let walker = WalkBuilder::new(search_dir)
        .overrides(overrides)
        .hidden(false)
        .build();

    let mut files: Vec<(PathBuf, SystemTime)> = Vec::new();
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("glob walk error: {}", e);
                continue;
            }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let mtime = match entry.metadata() {
            Ok(m) => match m.modified() {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("failed to get mtime for {}: {}", entry.path().display(), e);
                    SystemTime::UNIX_EPOCH
                }
            },
            Err(e) => {
                tracing::warn!(
                    "failed to get metadata for {}: {}",
                    entry.path().display(),
                    e
                );
                SystemTime::UNIX_EPOCH
            }
        };
        let path = entry.into_path();
        files.push((path, mtime));
    }

    // Sort by mtime descending (most recently modified first)
    files.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(files)
}

/// Fast file pattern matching tool that works with any codebase size.
/// Supports glob patterns like "**/*.js" or "src/**/*.ts".
/// Returns matching file paths sorted by modification time.
/// Use this tool when you need to find files by name patterns.
/// When you are doing an open-ended search that may require multiple rounds of globbing
/// and grepping, delegate to a subagent instead.
/// You have the capability to call multiple tools in a single response. It is always
/// better to speculatively perform multiple searches as a batch that are potentially useful.
#[tool]
pub fn glob(
    ctx: ToolContext,
    /// The glob pattern to match files against
    pattern: String,
    /// Directory to search in (defaults to current working directory)
    #[serde(default)]
    path: Option<String>,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        // Resolve current working directory once
        let cwd = match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                yield Err(anyhow!("Failed to get current directory: {}", e));
                return;
            }
        };

        // Resolve search directory
        let search_dir = match &path {
            Some(p) => PathBuf::from(p),
            None => cwd.clone(),
        };

        // Check if path is a directory (async)
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

        // Permission check for paths outside cwd
        if let Err(e) = crate::file_permission::check_file_read_permission(
            &ctx.permission, &search_dir, true,
        ).await {
            yield Err(e);
            return;
        }

        // Run the synchronous directory walk on a blocking thread
        let search_dir_clone = search_dir.clone();
        let pattern_clone = pattern.clone();
        let walk_result = tokio::task::spawn_blocking(move || {
            walk_glob(&search_dir_clone, &pattern_clone)
        }).await;

        let files = match walk_result {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                yield Err(e);
                return;
            }
            Err(e) => {
                yield Err(anyhow!("Glob walk task failed: {}", e));
                return;
            }
        };

        if files.is_empty() {
            yield Ok("No files found".to_string());
            return;
        }

        let total = files.len();
        let truncated = total > MAX_RESULTS;

        let mut output = files.iter()
            .take(MAX_RESULTS)
            .map(|(p, _)| p.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("\n");

        if truncated {
            output.push_str(&format!(
                "\n\n(Results truncated: showing {} of {} total matches)",
                MAX_RESULTS, total
            ));
        }

        yield Ok(output);
    }
}
