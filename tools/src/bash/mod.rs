pub mod command_parser;
pub mod command_permission;

#[cfg(test)]
mod bash_tests;
#[cfg(test)]
mod builtin_violation_tests;
#[cfg(test)]
mod command_parser_tests;
#[cfg(test)]
mod command_permission_tests;
#[cfg(test)]
mod output_processing_tests;

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use llm_rs::llm::{ChatOptions, LLMMessage};
use llm_rs::tool::{CancellationToken, ContainerConfig, ToolContext};
use llm_rs_macros::tool;
use regex::Regex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use uuid::Uuid;

use command_permission::check_bash_permission;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const POST_KILL_DRAIN_TIMEOUT_MS: u64 = 200;

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
/// is appended. If truncation occurs, the full raw output is saved to a file (see
/// Truncation markers).
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
/// When a `filter` is applied and it drops lines, the full raw output is saved
/// to a log file containing the complete raw output (before any processing);
/// the LLM can `read`/`grep`/`glob` it instead of re-running the command
/// with different filter/head/tail parameters.
///
/// ## Truncation markers
///
/// Whenever post-processing drops content, an inline marker is emitted so you
/// know to react (narrower `filter`, smaller `head` / `tail`):
///
/// - `[... earlier lines omitted by tail=K ...]` — top of output
/// - `[... later lines omitted by head=K ...]` — bottom of output
/// - `[... output truncated by chars_limit=C ...]` — bottom
/// - `[filter kept K/T lines]` — bottom (only when filter dropped some)
/// - `[filter matched 0/T lines — command produced output but none matched]`
///
/// Marker counts are intentionally simple; filter counts are exact, while
/// head/tail/char-cap markers only report that truncation happened. Markers do
/// not count toward the char cap, so a tight cap still shows actual content.
///
/// When output is truncated or filtered (head, tail, char cap, or filter drops
/// lines), the full raw tagged output is saved to a log file
/// under the session's tool-logs directory. The file path is emitted as
/// `log_file` in the `<bash_metadata>` block at the end of the output. The
/// LLM can then use `read`/`grep`/`glob` to search the full output instead
/// of re-running the command.
///
/// ## Other rules
///
/// - Before creating files/dirs, verify parent dir exists. Quote paths with spaces.
/// - Optional timeout, default 120000ms (2 min). Always provide a 5-10 word description.
/// - Use `workdir` instead of `cd <dir> && <command>`.
/// - Never use bash for `ls`, `find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, `echo` — use `read` for files/directories, `glob` for recursive patterns, `grep` for content search.
/// - Multiple commands: parallel tool calls for independent; `&&` for sequential dependent; `;` for sequential independent.
#[tool(self_managed_cancellation = true)]
#[allow(clippy::too_many_arguments)]
pub fn bash(
    ctx: ToolContext,
    /// The command to execute
    command: String,
    /// Bypass the automatic built-in-tool review. This should almost never be used.
    /// ONLY set to true when you have exhaustively verified that no combination of
    /// built-in tools (`read`, `glob`, `grep`) and bash parameters (`filter`, `head`,
    /// `tail`) can replace the shell utilities in your command. If there is ANY doubt,
    /// use the built-in tools instead. Misuse will be flagged.
    #[serde(default)]
    skip_auto_review: bool,
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
    /// exclusive with `tail`. Must be > 0. A bottom marker reports that later
    /// lines were dropped.
    #[serde(default)]
    head: Option<u64>,
    /// Keep only the last N output lines (applied AFTER `filter`). Mutually
    /// exclusive with `head`. Must be > 0. A top marker reports that earlier
    /// lines were dropped.
    #[serde(default)]
    tail: Option<u64>,
    /// Human-readable 5-10 word description of what this command does
    description: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let container_config = ctx.container_config.clone();

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

        if ctx.cancel_token.is_cancelled() {
            yield Err(anyhow!("Command cancelled"));
            return;
        }

        // Auto-review: check if the command uses shell utilities that have
        // built-in alternatives. The review LLM can only deny (return error)
        // or pass through to the permission check. It cannot auto-approve.
        if !skip_auto_review
            && has_reviewable_keywords(&command)
            && let Some(ref llm) = ctx.llm
            && let Some(ref model) = ctx.model
            && let Some(reason) = review_bash_command(llm.as_ref(), model, &command, &ctx.cancel_token).await
        {
            yield Err(anyhow!(
                "The command was auto-denied after review: {}\n\
                 \n\
                 Built-in tools and bash parameters must be used instead of raw shell utilities:\n\
                 - `read` tool → file contents and directory listings\n\
                 - `glob` tool → recursive file pattern matching\n\
                 - `grep` tool → content search in files\n\
                 - `filter` / `head` / `tail` params → output filtering and trimming\n\
                 \n\
                 Do NOT retry with `skip_auto_review: true` as a shortcut. Only use it if you\n\
                 have thoroughly verified that no combination of built-in tools and bash\n\
                 parameters can accomplish what this shell command does. If you are unsure,\n\
                 ask the user for guidance instead.",
                reason
            ));
            return;
        }

        if ctx.cancel_token.is_cancelled() {
            yield Err(anyhow!("Command cancelled"));
            return;
        }

        // If skip_auto_review is true AND the command has reviewable keywords,
        // emit a warning log for observability.
        if skip_auto_review && has_reviewable_keywords(&command) {
            tracing::warn!(
                command = %command,
                "skip_auto_review used with reviewable keywords"
            );
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

        // Normalize optional parameters: some models (notably OpenAI) always fill
        // in every schema field, sending `0` for unused integer options and `""` for
        // unused string options instead of omitting them.  Treat these as `None`.
        let timeout = timeout.filter(|&v| v > 0);
        let head = head.filter(|&v| v > 0);
        let tail = tail.filter(|&v| v > 0);
        let filter = filter.filter(|s| !s.is_empty());

        // Validate post-processing parameters BEFORE spawning the process —
        // never waste a 2-minute `cargo test` run on a bad parameter.
        if head.is_some() && tail.is_some() {
            yield Err(anyhow!("'head' and 'tail' are mutually exclusive"));
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

        if ctx.cancel_token.is_cancelled() {
            yield Err(anyhow!("Command cancelled"));
            return;
        }

        let request = BashRequest {
            command,
            description,
            timeout_ms: timeout.unwrap_or(DEFAULT_TIMEOUT_MS),
            work_dir,
            filter: compiled_filter,
            head,
            tail,
            container_config,
            cancel_token: ctx.cancel_token.clone(),
            tool_log_dir: ctx.session_dir.as_ref().map(|d| d.tool_log_dir()),
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        let _supervisor_task = tokio::spawn(run_bash_supervisor(request, tx));

        while let Some(item) = rx.recv().await {
            yield item;
        }
    }
}

type TaggedLineStream = std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<String>> + Send>>;

struct BashRequest {
    command: String,
    description: String,
    timeout_ms: u64,
    work_dir: Option<PathBuf>,
    filter: Option<Regex>,
    head: Option<u64>,
    tail: Option<u64>,
    container_config: Option<Arc<ContainerConfig>>,
    cancel_token: CancellationToken,
    tool_log_dir: Option<PathBuf>,
}

fn build_command(request: &BashRequest, job_id: Option<&str>) -> Result<Command> {
    if let Some(ref config) = request.container_config {
        let command = if let Some(job_id) = job_id {
            format!("( {} \n) # TCODE_JOB={}", request.command, job_id)
        } else {
            request.command.clone()
        };

        let mut cmd = Command::new(&config.runtime);
        cmd.arg("exec")
            .arg("--user")
            .arg(format!("{}:{}", config.uid, config.gid))
            .arg("-e")
            .arg(format!("HOME={}", config.home))
            .arg("-w");

        if let Some(ref dir) = request.work_dir {
            cmd.arg(dir.as_os_str());
        } else {
            let dir = std::env::current_dir().map_err(|e| {
                anyhow!("Failed to determine current directory for container workdir: {e}")
            })?;
            cmd.arg(dir);
        }

        cmd.arg(&config.name)
            .arg("bash")
            .arg("-c")
            .arg(&command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .process_group(0);
        Ok(cmd)
    } else {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(&request.command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .process_group(0);

        if let Some(ref dir) = request.work_dir {
            cmd.current_dir(dir);
        }
        Ok(cmd)
    }
}

struct OutputSink {
    tx: mpsc::UnboundedSender<Result<String>>,
    open: bool,
}

impl OutputSink {
    fn new(tx: mpsc::UnboundedSender<Result<String>>) -> Self {
        Self { tx, open: true }
    }

    fn send(&mut self, item: Result<String>) {
        if !self.open {
            return;
        }
        match self.tx.send(item) {
            Ok(()) => {}
            Err(_closed) => {
                self.open = false;
            }
        }
    }
}

async fn drain_remaining_output(
    line_stream: &mut TaggedLineStream,
    reducer: &mut OutputReducer,
    output: &mut OutputSink,
    log_file: &mut Option<(PathBuf, std::io::BufWriter<std::fs::File>)>,
) -> Result<()> {
    while let Some(line) = line_stream.next().await {
        let line = line?;
        if let Some((_, writer)) = log_file
            && let Err(e) = writeln!(writer, "{}", line)
        {
            tracing::warn!("Failed to write to bash log file: {e}");
        }
        if let Some(chunk) = reducer.feed(line) {
            output.send(Ok(chunk));
        }
    }
    Ok(())
}

async fn drain_remaining_output_with_timeout(
    line_stream_done: &mut bool,
    line_stream: &mut TaggedLineStream,
    reducer: &mut OutputReducer,
    output: &mut OutputSink,
    log_file: &mut Option<(PathBuf, std::io::BufWriter<std::fs::File>)>,
) {
    if *line_stream_done {
        return;
    }

    let drain_timeout = std::time::Duration::from_millis(POST_KILL_DRAIN_TIMEOUT_MS);
    match tokio::time::timeout(
        drain_timeout,
        drain_remaining_output(line_stream, reducer, output, log_file),
    )
    .await
    {
        Ok(Ok(())) => {
            *line_stream_done = true;
        }
        Ok(Err(e)) => {
            tracing::warn!("Failed to drain process output after termination: {}", e);
        }
        Err(_elapsed) => {
            tracing::warn!(
                "Timed out draining process output after {}ms",
                POST_KILL_DRAIN_TIMEOUT_MS
            );
        }
    }
}

async fn run_bash_supervisor(request: BashRequest, tx: mpsc::UnboundedSender<Result<String>>) {
    let mut output = OutputSink::new(tx);
    if request.cancel_token.is_cancelled() {
        output.send(Err(anyhow!("Command cancelled")));
        return;
    }

    let container_mode = request.container_config.is_some();
    let job_id = if container_mode {
        Some(Uuid::new_v4().to_string())
    } else {
        None
    };
    let mut cmd_builder = match build_command(&request, job_id.as_deref()) {
        Ok(cmd) => cmd,
        Err(e) => {
            output.send(Err(e));
            return;
        }
    };

    if request.cancel_token.is_cancelled() {
        output.send(Err(anyhow!("Command cancelled")));
        return;
    }

    let mut child = match cmd_builder.spawn() {
        Ok(child) => child,
        Err(e) => {
            if container_mode {
                output.send(Err(anyhow!("Failed to spawn container exec: {e}")));
            } else {
                output.send(Err(anyhow!("Failed to spawn bash: {e}")));
            }
            return;
        }
    };

    let pid = child.id();
    let mut guard = ChildGuard::new(pid, request.container_config.clone(), job_id.clone());
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut line_stream = process_line_stream(stdout, stderr);
    let mut wait_fut = Box::pin(child.wait());

    let description = request.description;
    let timeout_ms = request.timeout_ms;
    let timeout_duration = std::time::Duration::from_millis(timeout_ms);
    let deadline = tokio::time::Instant::now() + timeout_duration;
    let container_config = request.container_config;
    let cancel_token = request.cancel_token;
    let mut reducer = OutputReducer::new(
        request.filter,
        request.head,
        request.tail,
        DEFAULT_MAX_OUTPUT_CHARS,
    );

    let mut log_file: Option<(PathBuf, std::io::BufWriter<std::fs::File>)> = None;
    if let Some(ref tool_log_dir) = request.tool_log_dir {
        if let Err(e) = std::fs::create_dir_all(tool_log_dir) {
            tracing::warn!("Failed to create tool log directory: {e}");
        }
        let log_path = tool_log_dir.join(format!("bash-{}.log", Uuid::new_v4()));
        match std::fs::File::create(&log_path) {
            Ok(file) => {
                log_file = Some((log_path, std::io::BufWriter::new(file)));
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to create bash log file {}: {}",
                    log_path.display(),
                    e
                );
            }
        }
    }

    let mut line_stream_done = false;
    let mut child_done = false;
    let mut exit_code: Option<i32> = None;
    let mut stream_error: Option<anyhow::Error> = None;

    loop {
        if line_stream_done && child_done {
            break;
        }

        let timeout = tokio::time::sleep_until(deadline);
        tokio::pin!(timeout);

        tokio::select! {
            line = line_stream.next(), if !line_stream_done => {
                match line {
                    Some(Ok(line)) => {
                        if let Some((_, ref mut writer)) = log_file
                            && let Err(e) = writeln!(writer, "{}", line)
                        {
                            tracing::warn!("Failed to write to bash log file: {e}");
                        }
                        if let Some(chunk) = reducer.feed(line) {
                            output.send(Ok(chunk));
                        }
                    }
                    Some(Err(e)) => {
                        if stream_error.is_none() {
                            stream_error = Some(e);
                        }
                    }
                    None => {
                        line_stream_done = true;
                    }
                }
            }
            status = &mut wait_fut, if !child_done => {
                child_done = true;
                guard.disarm();
                match status {
                    Ok(status) => {
                        exit_code = status.code();
                    }
                    Err(e) => {
                        tracing::warn!("Failed to wait for child process: {}", e);
                    }
                }
            }
            () = &mut timeout => {
                let err = anyhow!("Command timed out after {}ms", timeout_ms);
                terminate_child_process_group(
                    pid,
                    &mut child_done,
                    &mut wait_fut,
                    &mut guard,
                    "timeout",
                    container_config.as_deref(),
                    job_id.as_deref(),
                )
                .await;
                drain_remaining_output_with_timeout(
                    &mut line_stream_done,
                    &mut line_stream,
                    &mut reducer,
                    &mut output,
                    &mut log_file,
                )
                .await;
                if let Some(final_output) = reducer.final_output() {
                    output.send(Ok(final_output));
                }
                let log_path_for_metadata = finalize_log_file(&mut log_file, &reducer);
                output.send(Ok(format_metadata(
                    "null",
                    &description,
                    Some(&err),
                    log_path_for_metadata.as_deref(),
                )));
                return;
            }
            () = cancel_token.cancelled() => {
                let err = anyhow!("Command cancelled");
                terminate_child_process_group(
                    pid,
                    &mut child_done,
                    &mut wait_fut,
                    &mut guard,
                    "cancellation",
                    container_config.as_deref(),
                    job_id.as_deref(),
                )
                .await;
                drain_remaining_output_with_timeout(
                    &mut line_stream_done,
                    &mut line_stream,
                    &mut reducer,
                    &mut output,
                    &mut log_file,
                )
                .await;
                if let Some(final_output) = reducer.final_output() {
                    output.send(Ok(final_output));
                }
                let log_path = finalize_log_file(&mut log_file, &reducer);
                let err = if let Some(ref p) = log_path {
                    err.context(format!("Full command output log saved to: {p}"))
                } else {
                    err
                };
                output.send(Err(err));
                return;
            }
        }
    }

    if let Some(e) = stream_error {
        if let Some(final_output) = reducer.final_output() {
            output.send(Ok(final_output));
        }
        let log_path = finalize_log_file(&mut log_file, &reducer);
        let e = if let Some(ref p) = log_path {
            e.context(format!("Full command output log saved to: {p}"))
        } else {
            e
        };
        output.send(Err(e));
        return;
    }

    if let Some(ref config) = container_config {
        let is_nonzero = matches!(exit_code, Some(code) if code != 0) || exit_code.is_none();
        if is_nonzero {
            let tail_stderr = reducer
                .stderr_lines()
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            if is_container_stopped_error(&tail_stderr) {
                if let Some(final_output) = reducer.final_output() {
                    output.send(Ok(final_output));
                }
                let log_path = finalize_log_file(&mut log_file, &reducer);
                let mut err = anyhow!(
                    "Error: Container '{}' is no longer running. \
                     Ask the user to restart the container before continuing.",
                    config.name
                );
                if let Some(ref p) = log_path {
                    err = err.context(format!("Full command output log saved to: {p}"));
                }
                output.send(Err(err));
                return;
            }
        }
    }

    if let Some(final_output) = reducer.final_output() {
        output.send(Ok(final_output));
    }

    let exit_str = match exit_code {
        Some(code) => code.to_string(),
        None => "null".to_string(),
    };
    let log_path_for_metadata = finalize_log_file(&mut log_file, &reducer);
    output.send(Ok(format_metadata(
        &exit_str,
        &description,
        None,
        log_path_for_metadata.as_deref(),
    )));
}

/// Flush and close the log file writer. Returns the canonicalized absolute
/// path if truncation occurred and the file should be kept, or `None` if
/// the file should be deleted (no truncation or no log file at all).
fn finalize_log_file(
    log_file: &mut Option<(PathBuf, std::io::BufWriter<std::fs::File>)>,
    reducer: &OutputReducer,
) -> Option<String> {
    let (log_path, mut writer) = log_file.take()?;
    if let Err(e) = writer.flush() {
        tracing::warn!("Failed to flush bash log file: {e}");
    }
    drop(writer);
    if reducer.had_truncation() {
        Some(
            std::fs::canonicalize(&log_path)
                .unwrap_or_else(|_| log_path.clone())
                .to_string_lossy()
                .to_string(),
        )
    } else {
        if let Err(e) = std::fs::remove_file(&log_path) {
            tracing::warn!(
                "Failed to remove non-truncated bash log file {}: {e}",
                log_path.display()
            );
        }
        None
    }
}

/// Check if a command contains any shell utility keywords that have built-in alternatives.
///
/// Uses word-boundary regex `\b{word}\b` for each keyword. This may produce false
/// positives on filenames like `grep-test` (since `\b` treats `-` as non-word boundary),
/// which is acceptable — the review LLM will correctly respond CONTINUE for those.
const REVIEWABLE_KEYWORDS: &[&str] = &[
    "ls", "find", "grep", "rg", "cat", "head", "tail", "sed", "awk", "echo", "2>&1",
];

fn has_reviewable_keywords(command: &str) -> bool {
    REVIEWABLE_KEYWORDS.iter().any(|kw| {
        let pattern = format!(r"\b{}\b", regex::escape(kw));
        Regex::new(&pattern).is_ok_and(|re| re.is_match(command))
    })
}

/// Send a bash command to the review LLM to check if it should use built-in tools instead.
///
/// Returns `Some(reason)` if the LLM denies the command, or `None` if the command
/// should continue to the permission check.
async fn review_bash_command(
    llm: &dyn llm_rs::llm::LLM,
    model: &str,
    command: &str,
    cancel_token: &CancellationToken,
) -> Option<String> {
    let prompt = format!(
        "You are a bash command reviewer. Your job is to check whether a bash command\n\
         unnecessarily uses shell utilities that have built-in tool alternatives.\n\
         \n\
         Built-in tools and bash parameters available:\n\
         - `read` tool: lists directory contents (replaces `ls`), reads file contents (replaces `cat`)\n\
         - `glob` tool: recursive file pattern matching (replaces `find`)\n\
         - `grep` tool: content search in files (replaces `grep`, `rg`)\n\
         - bash `filter` parameter: filters command output line-by-line (replaces `grep`, `rg`, `sed`, `awk` in pipelines)\n\
         - bash `head` parameter: keeps first N lines (replaces `head` in pipelines)\n\
         - bash `tail` parameter: keeps last N lines (replaces `tail` in pipelines)\n\
         - Direct response: for `echo`, the LLM should respond directly instead of using bash\n\
         - `2>&1` is NEVER needed — the bash tool automatically captures and merges both\n\
           stdout and stderr (each line is tagged `stdout| ` or `stderr| `). If the command\n\
           uses `2>&1` solely to merge stderr into stdout, respond with DENY. The only\n\
           exception is when `2>&1` is part of a complex shell pipeline that cannot be\n\
           expressed using `filter`/`head`/`tail` alone (e.g., `sort | uniq -c`).\n\
         \n\
         Review this bash command:\n\
         ```\n\
         {}\n\
         ```\n\
         \n\
         If the command can be accomplished using the built-in tools or bash parameters above,\n\
         respond with exactly:\n\
         DENY: <brief reason explaining which built-in tool/param to use instead>\n\
         \n\
         If the command genuinely requires bash (e.g., package managers, git, docker, compilers,\n\
         or complex shell operations that can't be replaced by built-in tools), respond with exactly:\n\
         CONTINUE\n\
         \n\
         Do not include any other text. Start your response with either DENY: or CONTINUE.",
        command
    );

    let msgs = vec![LLMMessage::User(vec![llm_rs::media::ContentPart::Text(
        prompt,
    )])];
    let options = ChatOptions::default();

    let mut stream = llm.chat(model, &msgs, &options);
    let mut response = String::new();
    loop {
        tokio::select! {
            event = stream.next() => {
                match event {
                    Some(llm_rs::llm::LLMEvent::TextDelta(text)) => response.push_str(&text),
                    Some(llm_rs::llm::LLMEvent::Error(e)) => {
                        tracing::warn!("LLM review error: {}; allowing command to proceed", e);
                        return None;
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            () = cancel_token.cancelled() => {
                return None;
            }
        }
    }

    // Check if response starts with "DENY:" (case-insensitive)
    if response.trim_start().to_lowercase().starts_with("deny:") {
        let reason = response.trim_start()["DENY:".len()..].trim().to_string();
        Some(reason)
    } else {
        // Anything else (CONTINUE, malformed, empty, etc.) — allow the command
        None
    }
}

enum OutputMode {
    Stream {
        emitted_lines: u64,
        emitted_chars: usize,
        cap_hit: bool,
    },
    Tail {
        limit: u64,
        lines: VecDeque<String>,
        tail_hit: bool,
    },
}

/// Pure output reducer shared by live streaming and buffered tail output.
pub(crate) struct OutputReducer {
    filter: Option<Regex>,
    head: Option<u64>,
    max_chars: usize,
    raw_total: u64,
    kept_total: u64,
    last_stderr: VecDeque<String>,
    mode: OutputMode,
    truncated: bool,
}

impl OutputReducer {
    pub(crate) fn new(
        filter: Option<Regex>,
        head: Option<u64>,
        tail: Option<u64>,
        max_chars: usize,
    ) -> Self {
        let mode = if let Some(limit) = tail {
            OutputMode::Tail {
                limit,
                lines: VecDeque::new(),
                tail_hit: false,
            }
        } else {
            OutputMode::Stream {
                emitted_lines: 0,
                emitted_chars: 0,
                cap_hit: false,
            }
        };

        Self {
            filter,
            head,
            max_chars,
            raw_total: 0,
            kept_total: 0,
            last_stderr: VecDeque::with_capacity(5),
            mode,
            truncated: false,
        }
    }

    pub(crate) fn feed(&mut self, line: String) -> Option<String> {
        self.raw_total += 1;
        if let Some(s) = line.strip_prefix(STDERR_TAG) {
            if self.last_stderr.len() == 5 {
                self.last_stderr.pop_front();
            }
            self.last_stderr.push_back(s.to_string());
        }

        if let Some(ref re) = self.filter
            && !re.is_match(&line)
        {
            return None;
        }

        self.kept_total += 1;
        match &mut self.mode {
            OutputMode::Tail {
                limit,
                lines,
                tail_hit,
            } => {
                lines.push_back(line);
                if u64::try_from(lines.len()).unwrap_or(u64::MAX) > *limit {
                    lines.pop_front();
                    *tail_hit = true;
                    self.truncated = true;
                }
                None
            }
            OutputMode::Stream {
                emitted_lines,
                emitted_chars,
                cap_hit,
            } => {
                if let Some(head) = self.head
                    && self.kept_total > head
                {
                    self.truncated = true;
                    return None;
                }

                if *cap_hit {
                    return None;
                }

                let line_chars = line.chars().count();
                let added_chars = if *emitted_lines == 0 {
                    line_chars
                } else {
                    line_chars.saturating_add(1)
                };
                if emitted_chars.saturating_add(added_chars) > self.max_chars {
                    *cap_hit = true;
                    self.truncated = true;
                    return None;
                }

                let output = if *emitted_lines == 0 {
                    line
                } else {
                    format!("\n{line}")
                };
                *emitted_lines += 1;
                *emitted_chars += added_chars;
                Some(output)
            }
        }
    }

    pub(crate) fn final_output(&mut self) -> Option<String> {
        enum FinalMode {
            Stream {
                emitted_any: bool,
                cap_hit: bool,
            },
            Tail {
                top_marker: Option<String>,
                content: String,
                cap_hit: bool,
            },
        }

        let final_mode = match &mut self.mode {
            OutputMode::Stream {
                emitted_lines,
                cap_hit,
                ..
            } => FinalMode::Stream {
                emitted_any: *emitted_lines > 0,
                cap_hit: *cap_hit,
            },
            OutputMode::Tail {
                limit,
                lines,
                tail_hit,
            } => {
                let (content, cap_hit) =
                    apply_char_cap_from_end(lines.iter().cloned().collect(), self.max_chars);
                if cap_hit {
                    self.truncated = true;
                }
                let top_marker = if *tail_hit {
                    Some(format!("[... earlier lines omitted by tail={} ...]", limit))
                } else {
                    None
                };
                FinalMode::Tail {
                    top_marker,
                    content,
                    cap_hit,
                }
            }
        };

        match final_mode {
            FinalMode::Stream {
                emitted_any,
                cap_hit,
            } => {
                let markers = self.bottom_markers(cap_hit);
                if markers.is_empty() {
                    None
                } else {
                    let joined = markers.join("\n");
                    Some(if emitted_any {
                        format!("\n{joined}")
                    } else {
                        joined
                    })
                }
            }
            FinalMode::Tail {
                top_marker,
                content,
                cap_hit,
            } => {
                let mut output = String::new();
                if let Some(marker) = top_marker {
                    output.push_str(&marker);
                }
                if !content.is_empty() {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&content);
                }
                for marker in self.bottom_markers(cap_hit) {
                    if !output.is_empty() && !output.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str(&marker);
                }
                if output.is_empty() {
                    None
                } else {
                    Some(output)
                }
            }
        }
    }

    fn bottom_markers(&self, cap_hit: bool) -> Vec<String> {
        let mut markers = Vec::new();
        if let Some(head) = self.head
            && self.kept_total > head
        {
            markers.push(format!("[... later lines omitted by head={} ...]", head));
        }
        if cap_hit {
            markers.push(format!(
                "[... output truncated by chars_limit={} ...]",
                self.max_chars
            ));
        }
        if self.filter.is_some() {
            if self.kept_total == 0 && self.raw_total > 0 {
                markers.push(format!(
                    "[filter matched 0/{} lines — command produced output but none matched]",
                    self.raw_total
                ));
            } else if self.kept_total < self.raw_total {
                markers.push(format!(
                    "[filter kept {}/{} lines]",
                    self.kept_total, self.raw_total
                ));
            }
        }
        markers
    }

    fn stderr_lines(&self) -> &VecDeque<String> {
        &self.last_stderr
    }

    /// Returns true if any truncation or filtering occurred during processing.
    ///
    /// Must be called after [`final_output`](OutputReducer::final_output), because
    /// tail-mode char-cap truncation is only detected during final output assembly.
    pub(crate) fn had_truncation(&self) -> bool {
        self.truncated || (self.filter.is_some() && self.kept_total < self.raw_total)
    }
}

/// Merge tagged stdout and stderr lines into a single stream.
/// Yields each tagged line as it arrives from the process.
fn process_line_stream(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
) -> TaggedLineStream {
    fn lines_stream<R: tokio::io::AsyncBufRead + Unpin + Send + 'static>(
        reader: R,
        tag: &'static str,
    ) -> TaggedLineStream {
        Box::pin(
            tokio_stream::wrappers::LinesStream::new(reader.lines()).map(move |l| {
                l.map(|line| format!("{}{}", tag, line))
                    .map_err(anyhow::Error::from)
            }),
        )
    }

    let stdout_stream = stdout.map(|s| lines_stream(BufReader::new(s), STDOUT_TAG));
    let stderr_stream = stderr.map(|s| lines_stream(BufReader::new(s), STDERR_TAG));

    match (stdout_stream, stderr_stream) {
        (Some(out), Some(err)) => Box::pin(StreamExt::merge(out, err)),
        (Some(s), None) => s,
        (None, Some(s)) => s,
        (None, None) => Box::pin(tokio_stream::empty()),
    }
}

/// Apply the post-processing pipeline to collected, tagged output lines.
///
/// Pure (no I/O) so it can be unit-tested exhaustively without spawning a
/// process. This uses the same reducer as live command output.
#[cfg(test)]
pub(crate) fn post_process(
    lines: Vec<String>,
    filter: Option<&Regex>,
    head: Option<u64>,
    tail: Option<u64>,
    max_chars: usize,
) -> String {
    let mut reducer = OutputReducer::new(filter.cloned(), head, tail, max_chars);
    let mut output = String::new();
    for line in lines {
        if let Some(chunk) = reducer.feed(line) {
            output.push_str(&chunk);
        }
    }
    if let Some(final_output) = reducer.final_output() {
        output.push_str(&final_output);
    }
    output
}

/// Truncate `lines` from the front so the joined content fits in `max_chars`,
/// preserving the newest lines. Used by `tail` so a char cap does not defeat
/// the caller's request for the last lines.
fn apply_char_cap_from_end(lines: Vec<String>, max_chars: usize) -> (String, bool) {
    let line_chars: Vec<usize> = lines.iter().map(|line| line.chars().count()).collect();
    let total_chars = line_chars
        .iter()
        .fold(lines.len().saturating_sub(1), |acc, len| {
            acc.saturating_add(*len)
        });

    if total_chars <= max_chars {
        return (lines.join("\n"), false);
    }

    let mut acc_chars = 0usize;
    let mut kept = Vec::new();
    for (idx, line) in lines.into_iter().enumerate().rev() {
        let added = if kept.is_empty() {
            line_chars[idx]
        } else {
            line_chars[idx].saturating_add(1)
        };
        if acc_chars.saturating_add(added) > max_chars {
            break;
        }
        acc_chars = acc_chars.saturating_add(added);
        kept.push(line);
    }
    kept.reverse();

    (kept.join("\n"), true)
}

fn process_group_id(pid: Option<u32>) -> Option<nix::unistd::Pid> {
    let pid = pid?;
    let pid_i32 = match i32::try_from(pid) {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("PID {} overflows i32; cannot signal process group", pid);
            return None;
        }
    };
    Some(nix::unistd::Pid::from_raw(pid_i32))
}

fn send_process_group_signal(pid: Option<u32>, signal: nix::sys::signal::Signal) -> bool {
    let Some(pgid) = process_group_id(pid) else {
        return false;
    };

    match nix::sys::signal::killpg(pgid, signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => true,
        Err(e) => {
            tracing::warn!(
                "Failed to send {:?} to process group {:?}: {}",
                signal,
                pid,
                e
            );
            false
        }
    }
}

async fn terminate_child_process_group<F>(
    pid: Option<u32>,
    child_done: &mut bool,
    wait_fut: &mut F,
    guard: &mut ChildGuard,
    reason: &str,
    container_config: Option<&ContainerConfig>,
    job_id: Option<&str>,
) where
    F: std::future::Future<Output = std::io::Result<std::process::ExitStatus>> + Unpin,
{
    send_process_group_signal(pid, nix::sys::signal::Signal::SIGTERM);

    let mut escalated = false;
    if !*child_done {
        match tokio::time::timeout(std::time::Duration::from_secs(2), &mut *wait_fut).await {
            Ok(Ok(_status)) => {
                *child_done = true;
                guard.disarm();
            }
            Ok(Err(e)) => {
                tracing::warn!("Failed to wait for child process after {}: {}", reason, e);
                *child_done = true;
                guard.disarm();
            }
            Err(_elapsed) => {
                if send_process_group_signal(pid, nix::sys::signal::Signal::SIGKILL) {
                    tracing::warn!(
                        "Process group {:?} did not exit after SIGTERM for {}; sent SIGKILL",
                        pid,
                        reason
                    );
                    escalated = true;
                }
            }
        }
    }

    if escalated && !*child_done {
        match tokio::time::timeout(std::time::Duration::from_secs(2), &mut *wait_fut).await {
            Ok(Ok(_status)) => {
                *child_done = true;
                guard.disarm();
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    "Failed to wait for child process after SIGKILL for {}: {}",
                    reason,
                    e
                );
                *child_done = true;
                guard.disarm();
            }
            Err(_elapsed) => {
                tracing::warn!(
                    "Timed out waiting for child process after SIGKILL for {}",
                    reason
                );
            }
        }
    }

    if let (Some(config), Some(job_id)) = (container_config, job_id) {
        kill_container_process_group(config, job_id).await;
    }
}

async fn kill_container_process_group(config: &ContainerConfig, job_id: &str) {
    run_container_kill_signal(config, job_id, "TERM").await;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    run_container_kill_signal(config, job_id, "KILL").await;
}

async fn run_container_kill_signal(config: &ContainerConfig, job_id: &str, signal: &str) {
    let script = format!(
        r#"PID=$(pgrep -f "TCODE_JOB={job_id}" | grep -v "^$$\$" | sort -n | head -1); [ -n "$PID" ] && kill -{signal} -- -$(ps -o pgid= -p "$PID" | tr -d " ") || true"#,
        job_id = job_id,
        signal = signal,
    );

    let mut cmd = Command::new(&config.runtime);
    cmd.arg("exec")
        .arg("--user")
        .arg(format!("{}:{}", config.uid, config.gid))
        .arg(&config.name)
        .arg("bash")
        .arg("-c")
        .arg(&script)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output()).await;

    match result {
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let trimmed = stderr.trim();
            if !trimmed.is_empty() {
                tracing::info!(
                    "Container kill script stderr for job {} (SIG{}): {}",
                    job_id,
                    signal,
                    trimmed
                );
            }
        }
        Ok(Err(e)) => {
            tracing::warn!(
                "Failed to send SIG{} to container process group for job {}: {}",
                signal,
                job_id,
                e
            );
        }
        Err(_elapsed) => {
            tracing::warn!(
                "Timed out sending SIG{} to container process group for job {}",
                signal,
                job_id
            );
        }
    }
}

/// Send SIGTERM to a process group, then SIGKILL after a 2-second grace period
/// if the process group is still alive. Used as a best-effort guard cleanup
/// when the supervisor is dropped unexpectedly.
async fn kill_process_group(pid: Option<u32>) {
    let Some(pgid) = process_group_id(pid) else {
        return;
    };

    send_process_group_signal(pid, nix::sys::signal::Signal::SIGTERM);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if let Err(nix::errno::Errno::ESRCH) = nix::sys::signal::killpg(pgid, None) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
    }

    if send_process_group_signal(pid, nix::sys::signal::Signal::SIGKILL) {
        tracing::warn!(
            "Process group {:?} did not exit after SIGTERM; sent SIGKILL",
            pid
        );
    }
}

/// Kills the child process group when the supervisor is dropped unexpectedly.
struct ChildGuard {
    pid: Option<u32>,
    container_config: Option<Arc<ContainerConfig>>,
    job_id: Option<String>,
}

impl ChildGuard {
    fn new(
        pid: Option<u32>,
        container_config: Option<Arc<ContainerConfig>>,
        job_id: Option<String>,
    ) -> Self {
        Self {
            pid,
            container_config,
            job_id,
        }
    }

    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let container_config = self.container_config.clone();
            let job_id = self.job_id.clone();
            handle.spawn(async move {
                kill_process_group(Some(pid)).await;
                if let (Some(config), Some(job_id)) =
                    (container_config.as_deref(), job_id.as_deref())
                {
                    kill_container_process_group(config, job_id).await;
                }
            });
        } else if self.pid.is_some() {
            tracing::warn!(
                pid = ?self.pid,
                "ChildGuard dropped without a tokio runtime; child process may leak"
            );
        }
    }
}

/// Format the `<bash_metadata>` block appended to every tool response.
fn format_metadata(
    exit_code: &str,
    description: &str,
    error: Option<&anyhow::Error>,
    log_file: Option<&str>,
) -> String {
    let log_line = log_file
        .map(|p| format!("\nlog_file: {p} — complete raw output (before any filter/head/tail/char-cap truncation); use read/grep/glob to search instead of re-running the command"))
        .unwrap_or_default();
    match error {
        Some(e) => format!(
            "\n<bash_metadata>\nexit_code: {}\ndescription: {}\nerror: {}{}\n</bash_metadata>",
            exit_code, description, e, log_line
        ),
        None => format!(
            "\n<bash_metadata>\nexit_code: {}\ndescription: {}{}\n</bash_metadata>",
            exit_code, description, log_line
        ),
    }
}

/// Check if stderr output indicates the container has stopped or disappeared.
fn is_container_stopped_error(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("is not running")
        || lower.contains("no such container")
        || lower.contains("is restarting")
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
