pub mod command_parser;
pub mod command_permission;

#[cfg(test)]
mod bash_tests;
#[cfg(test)]
mod command_parser_tests;
#[cfg(test)]
mod command_permission_tests;
#[cfg(test)]
mod output_processing_tests;

use std::process::Stdio;

use anyhow::{Result, anyhow};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use command_permission::check_bash_permission;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Maximum characters of post-processed output returned per command. Hidden
/// from the LLM as a parameter — the LLM reacts to a hit by narrowing
/// `filter` / `head` / `tail`, not by asking for more chars. Markers
/// themselves do NOT count toward this cap.
pub(crate) const DEFAULT_MAX_OUTPUT_CHARS: usize = 20_000;

/// Prefix tag prepended to every stdout line before merging.
const STDOUT_TAG: &str = "stdout| ";

/// Prefix tag prepended to every stderr line before merging.
const STDERR_TAG: &str = "stderr| ";

/// Run a shell command and return its tagged, optionally filtered/trimmed output.
///
/// Use this for terminal operations only (git, npm, cargo, docker, etc.). NOT for
/// file ops — use `read`/`write`/`edit`/`grep`/`glob` for those.
///
/// ## Output format
///
/// Every line of output is prefixed with `stdout| ` or `stderr| ` so you can tell
/// which stream produced it. Both streams are captured automatically — you never
/// need `2>&1`. The total output is capped at a hidden character limit; when the
/// cap is hit, output is cut at the last newline boundary and a truncation marker
/// is appended.
///
/// ## Filtering and trimming (preferred over shell pipes)
///
/// Instead of piping through `grep`, `head`, `tail`, `sed`, etc., use the `filter`,
/// `head`, and `tail` parameters. The bash tool applies them server-side after
/// the command runs.
///
/// - `cmd 2>&1 | tail -n N`            → `bash(command="cmd", tail=N)`
/// - `cmd 2>&1 | head -n N`            → `bash(command="cmd", head=N)`
/// - `cmd 2>&1 | grep PAT`             → `bash(command="cmd", filter="PAT")`
/// - `cmd | grep -E "a|b" | tail -20`  → `bash(command="cmd", filter="a|b", tail=20)`
///
/// `filter` is a Rust `regex` crate pattern, applied line-by-line to the merged
/// (already-tagged) output. The prefix is part of each line, so you can match
/// on it: `filter="^stderr\\|"` isolates stderr; `filter="^stderr\\| .*error"`
/// isolates stderr error lines. Inline `(?i)` enables case-insensitivity.
///
/// `head` and `tail` are mutually exclusive — setting both is a validation error.
/// Both must be > 0. They run AFTER `filter`, so `tail=10` keeps the last 10
/// *matching* lines, not the last 10 raw lines.
///
/// ## Truncation markers
///
/// Whenever post-processing drops content, an inline marker is emitted so you
/// know to react (narrower `filter`, smaller `head` / `tail`):
///
/// - `[... N earlier lines omitted by tail=K ...]` — top of output
/// - `[... N later lines omitted by head=K ...]` — bottom of output
/// - `[... output truncated: N more chars omitted by chars_limit=C ...]` — bottom
/// - `[filter kept K/T lines]` — bottom (only when filter dropped some)
/// - `[filter matched 0/T lines — command produced output but none matched]`
///
/// Counts are real numbers, not "...more". Markers do not count toward the char
/// cap, so a tight cap still shows actual content.
///
/// ## Other rules
///
/// - Before creating files/dirs, verify parent dir exists. Quote paths with spaces.
/// - Optional timeout, default 120000ms (2 min). Always provide a 5-10 word description.
/// - Use `workdir` instead of `cd <dir> && <command>`.
/// - Never use bash for `ls`, `find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, `echo` — use `read` for files/directories, `glob` for recursive patterns, `grep` for content search.
/// - Multiple commands: parallel tool calls for independent; `&&` for sequential dependent; `;` for sequential independent.
#[tool]
#[allow(clippy::too_many_arguments)]
pub fn bash(
    ctx: ToolContext,
    /// The command to execute
    command: String,
    /// Timeout in milliseconds (default: 120000ms / 2 minutes)
    #[serde(default)]
    timeout: Option<u64>,
    /// Working directory for the command. If not specified, uses the current
    /// working directory. Use this instead of `cd <dir> && <command>`.
    #[serde(default)]
    workdir: Option<String>,
    /// Rust `regex` crate pattern applied line-by-line to the merged output
    /// AFTER tagging with `stdout| ` / `stderr| `. Non-matching lines are
    /// dropped. The prefix is visible to the regex, so `filter="^stderr\\|"`
    /// isolates stderr. Use inline `(?i)` for case-insensitivity. Invalid
    /// regex is rejected before the command runs.
    #[serde(default)]
    filter: Option<String>,
    /// Keep only the first N output lines (applied AFTER `filter`). Mutually
    /// exclusive with `tail`. Must be > 0. A bottom marker reports how many
    /// later lines were dropped.
    #[serde(default)]
    head: Option<u64>,
    /// Keep only the last N output lines (applied AFTER `filter`). Mutually
    /// exclusive with `head`. Must be > 0. A top marker reports how many
    /// earlier lines were dropped.
    #[serde(default)]
    tail: Option<u64>,
    /// Human-readable 5-10 word description of what this command does
    description: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        if command.trim().is_empty() {
            yield Err(anyhow!("command must not be empty"));
            return;
        }

        // Reject commands that start with `cd` — each command runs in its own
        // shell subprocess, so `cd` has no lasting effect and is always a mistake.
        if starts_with_cd(&command) {
            yield Err(anyhow!(
                "Do not use `cd` in commands — each command runs in its own shell, \
                 so `cd` has no lasting effect. Use the `workdir` parameter to set \
                 the working directory instead."
            ));
            return;
        }

        // Resolve working directory
        let work_dir = if let Some(ref dir) = workdir {
            let path = std::path::Path::new(dir);
            if !path.is_absolute() {
                yield Err(anyhow!("workdir must be an absolute path, got: {}", dir));
                return;
            }
            if !path.is_dir() {
                yield Err(anyhow!("workdir does not exist or is not a directory: {}", dir));
                return;
            }
            Some(path.to_path_buf())
        } else {
            None
        };

        // Validate post-processing parameters BEFORE spawning the process —
        // never waste a 2-minute `cargo test` run on a bad parameter.
        if head.is_some() && tail.is_some() {
            yield Err(anyhow!("'head' and 'tail' are mutually exclusive"));
            return;
        }
        if head == Some(0) {
            yield Err(anyhow!("'head' must be greater than 0"));
            return;
        }
        if tail == Some(0) {
            yield Err(anyhow!("'tail' must be greater than 0"));
            return;
        }
        let compiled_filter: Option<Regex> = match filter.as_deref() {
            Some(pattern) => match Regex::new(pattern) {
                Ok(re) => Some(re),
                Err(e) => {
                    yield Err(anyhow!("invalid 'filter' regex: {}", e));
                    return;
                }
            },
            None => None,
        };

        // Permission check
        if let Err(e) = check_bash_permission(&ctx.permission, &command, work_dir.as_deref()).await {
            yield Err(e);
            return;
        }

        let timeout_ms = timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
        let timeout_duration = std::time::Duration::from_millis(timeout_ms);

        // Spawn bash -c <command> with its own process group
        let mut cmd_builder = Command::new("bash");
        cmd_builder
            .arg("-c")
            .arg(&command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .process_group(0);

        if let Some(ref dir) = work_dir {
            cmd_builder.current_dir(dir);
        }

        let mut child = match cmd_builder.spawn() {
            Ok(c) => c,
            Err(e) => {
                yield Err(anyhow!("Failed to spawn bash: {}", e));
                return;
            }
        };

        let pid = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Merge tagged stdout and stderr lines into a single Vec<String>.
        let mut output: Vec<String> = Vec::new();

        let cancel_token = ctx.cancel_token.clone();

        // Read stdout and stderr concurrently with timeout
        let read_result = tokio::select! {
            result = async {
                read_process_output(stdout, stderr, &mut output).await
            } => result,
            () = tokio::time::sleep(timeout_duration) => {
                // Timeout — kill the process group
                kill_process_group(pid).await;
                Err(anyhow!("Command timed out after {}ms", timeout_ms))
            }
            () = cancel_token.cancelled() => {
                // Cancellation — kill the process group
                kill_process_group(pid).await;
                Err(anyhow!("Command cancelled"))
            }
        };

        let exit_code = match read_result {
            Ok(()) => {
                // Wait for the child to finish
                match child.wait().await {
                    Ok(status) => status.code(),
                    Err(e) => {
                        tracing::warn!("Failed to wait for child process: {}", e);
                        None
                    }
                }
            }
            Err(e) => {
                // Try to wait briefly for the child to clean up
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    child.wait()
                ).await;

                let processed = post_process(
                    output,
                    compiled_filter.as_ref(),
                    head,
                    tail,
                    DEFAULT_MAX_OUTPUT_CHARS,
                );
                if !processed.is_empty() {
                    yield Ok(processed);
                }

                yield Ok(format_metadata("null", &description, Some(&e)));
                return;
            }
        };

        // Run the post-processing pipeline before yielding the output.
        let processed = post_process(
            output,
            compiled_filter.as_ref(),
            head,
            tail,
            DEFAULT_MAX_OUTPUT_CHARS,
        );
        if !processed.is_empty() {
            yield Ok(processed);
        }

        // Yield metadata
        let exit_str = match exit_code {
            Some(code) => code.to_string(),
            None => "null".to_string(),
        };
        yield Ok(format_metadata(&exit_str, &description, None));
    }
}

/// Read stdout and stderr from a child process, tagging each line with
/// `stdout| ` / `stderr| ` and merging into a single `Vec<String>` (one
/// entry per line) in arrival order.
async fn read_process_output(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    output: &mut Vec<String>,
) -> Result<()> {
    use tokio_stream::StreamExt;

    fn lines_stream<R: tokio::io::AsyncBufRead + Unpin + Send>(
        reader: R,
        tag: &'static str,
    ) -> impl tokio_stream::Stream<Item = Result<String>> + Send {
        tokio_stream::wrappers::LinesStream::new(reader.lines()).map(move |l| {
            l.map(|line| format!("{}{}", tag, line))
                .map_err(anyhow::Error::from)
        })
    }

    let stdout_stream = stdout.map(|s| lines_stream(BufReader::new(s), STDOUT_TAG));
    let stderr_stream = stderr.map(|s| lines_stream(BufReader::new(s), STDERR_TAG));

    // Build a merged stream from whichever of stdout/stderr are available.
    let combined: Option<
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<String>> + Send + '_>>,
    > = match (stdout_stream, stderr_stream) {
        (Some(out), Some(err)) => Some(Box::pin(StreamExt::merge(out, err))),
        (Some(s), None) => Some(Box::pin(s)),
        (None, Some(s)) => Some(Box::pin(s)),
        (None, None) => None,
    };

    if let Some(mut stream) = combined {
        while let Some(line) = stream.next().await {
            output.push(line?);
        }
    }

    Ok(())
}

/// Apply the post-processing pipeline to the collected, tagged output lines:
/// `filter` → `tail` / `head` → char cap → markers.
///
/// Pure (no I/O) so it can be unit-tested exhaustively without spawning a
/// process. Marker rules follow plan.md "Truncation markers".
pub(crate) fn post_process(
    lines: Vec<String>,
    filter: Option<&Regex>,
    head: Option<u64>,
    tail: Option<u64>,
    max_chars: usize,
) -> String {
    let total = lines.len();

    // Step 5: filter (line-by-line, drop non-matches).
    let (filtered, kept_count): (Vec<String>, usize) = if let Some(re) = filter {
        let kept: Vec<String> = lines.into_iter().filter(|line| re.is_match(line)).collect();
        let n = kept.len();
        (kept, n)
    } else {
        let n = lines.len();
        (lines, n)
    };

    // Step 6: tail or head (mutually exclusive — validation rejects both).
    let (windowed, dropped_top, dropped_bottom) = if let Some(n) = tail {
        let n = n as usize;
        if filtered.len() > n {
            let dropped = filtered.len() - n;
            let kept: Vec<String> = filtered.into_iter().skip(dropped).collect();
            (kept, dropped, 0usize)
        } else {
            (filtered, 0usize, 0usize)
        }
    } else if let Some(n) = head {
        let n = n as usize;
        if filtered.len() > n {
            let dropped = filtered.len() - n;
            let kept: Vec<String> = filtered.into_iter().take(n).collect();
            (kept, 0usize, dropped)
        } else {
            (filtered, 0usize, 0usize)
        }
    } else {
        (filtered, 0usize, 0usize)
    };

    // Steps 7–8: join, then char-cap at the last newline boundary <= cap.
    let (content, dropped_chars) = apply_char_cap(windowed, max_chars);

    // Step 9: assemble markers. Tail marker on top; head / chars-limit /
    // filter markers at the bottom in that order. Markers are NOT counted
    // toward `max_chars`.
    let mut output = String::new();
    if dropped_top > 0 {
        let n = tail.unwrap_or(0);
        output.push_str(&format!(
            "[... {} earlier lines omitted by tail={} ...]",
            dropped_top, n
        ));
        output.push('\n');
    }
    output.push_str(&content);

    let mut bottom: Vec<String> = Vec::new();
    if dropped_bottom > 0 {
        let n = head.unwrap_or(0);
        bottom.push(format!(
            "[... {} later lines omitted by head={} ...]",
            dropped_bottom, n
        ));
    }
    if dropped_chars > 0 {
        bottom.push(format!(
            "[... output truncated: {} more chars omitted by chars_limit={} ...]",
            dropped_chars, max_chars
        ));
    }
    if filter.is_some() {
        if kept_count == 0 && total > 0 {
            bottom.push(format!(
                "[filter matched 0/{} lines — command produced output but none matched]",
                total
            ));
        } else if kept_count < total {
            bottom.push(format!("[filter kept {}/{} lines]", kept_count, total));
        }
    }
    for marker in bottom {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&marker);
    }

    // Strip a trailing newline that may have been left by the top marker
    // when content is empty (e.g. tail dropped everything because the cap
    // ate the last lines).
    while output.ends_with('\n') {
        output.pop();
    }

    output
}

/// Truncate `lines` so the joined content fits in `max_chars` characters,
/// cutting at the last newline boundary <= cap. Returns the kept content and
/// the number of characters dropped (0 when no truncation was needed).
fn apply_char_cap(lines: Vec<String>, max_chars: usize) -> (String, usize) {
    // Total chars of `lines.join("\n")`: sum of line char counts plus
    // (lines.len() - 1) separator newlines.
    let line_chars: Vec<usize> = lines.iter().map(|l| l.chars().count()).collect();
    let total_chars: usize = line_chars.iter().sum::<usize>() + lines.len().saturating_sub(1);

    if total_chars <= max_chars {
        return (lines.join("\n"), 0);
    }

    // Walk lines, accumulating until adding the next line would exceed cap.
    // This naturally cuts at the last newline boundary because we keep whole
    // lines only.
    let mut acc_chars: usize = 0;
    let mut kept: Vec<String> = Vec::with_capacity(lines.len());
    for (idx, line) in lines.into_iter().enumerate() {
        let added = if kept.is_empty() {
            line_chars[idx]
        } else {
            line_chars[idx] + 1 // +1 for the joining newline
        };
        if acc_chars + added > max_chars {
            break;
        }
        acc_chars += added;
        kept.push(line);
    }

    let content = kept.join("\n");
    let dropped = total_chars - acc_chars;
    (content, dropped)
}

/// Send SIGTERM to a process group, then SIGKILL after a 2-second grace period
/// if the process group is still alive.
async fn kill_process_group(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    let pid_i32 = match i32::try_from(pid) {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("PID {} overflows i32; cannot kill process group", pid);
            return;
        }
    };
    let pgid = nix::unistd::Pid::from_raw(pid_i32);

    // First attempt: graceful shutdown via SIGTERM
    if let Err(e) = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGTERM) {
        tracing::warn!("Failed to SIGTERM process group {}: {}", pid, e);
        return;
    }

    // Poll every 50ms for up to 2 seconds for the process group to exit on its own
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // killpg with signal=None probes whether the process group still exists;
        // ESRCH means it is gone — no need to escalate.
        if let Err(nix::errno::Errno::ESRCH) = nix::sys::signal::killpg(pgid, None) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }

    // Escalate to SIGKILL if still alive (ESRCH means it already exited)
    match nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL) {
        Ok(()) => {
            tracing::warn!(
                "Process group {} did not exit after SIGTERM; sent SIGKILL",
                pid
            );
        }
        Err(nix::errno::Errno::ESRCH) => {
            // Process group already exited during the grace period — nothing to do
        }
        Err(e) => {
            tracing::warn!("Failed to SIGKILL process group {}: {}", pid, e);
        }
    }
}

/// Format the `<bash_metadata>` block appended to every tool response.
fn format_metadata(exit_code: &str, description: &str, error: Option<&anyhow::Error>) -> String {
    match error {
        Some(e) => format!(
            "\n<bash_metadata>\nexit_code: {}\ndescription: {}\nerror: {}\n</bash_metadata>",
            exit_code, description, e
        ),
        None => format!(
            "\n<bash_metadata>\nexit_code: {}\ndescription: {}\n</bash_metadata>",
            exit_code, description
        ),
    }
}

/// Check if a command starts with `cd` as the first token.
fn starts_with_cd(command: &str) -> bool {
    let trimmed = command.trim_start();
    trimmed == "cd"
        || trimmed.starts_with("cd ")
        || trimmed.starts_with("cd\t")
        || trimmed.starts_with("cd;")
        || trimmed.starts_with("cd&")
        || trimmed.starts_with("cd\n")
}
