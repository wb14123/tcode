pub mod command_parser;
pub mod command_permission;

#[cfg(test)]
mod bash_tests;
#[cfg(test)]
mod command_parser_tests;
#[cfg(test)]
mod command_permission_tests;

use std::process::Stdio;

use anyhow::{Result, anyhow};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use command_permission::check_bash_permission;

const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Terminal operations only (git, npm, docker, etc.). NOT for file ops — use dedicated tools.
///
/// - Before creating files/dirs, verify parent dir exists. Quote paths with spaces.
/// - Optional timeout, default 120000ms (2 min). Always provide a 5-10 word description.
/// - Use `workdir` instead of `cd <dir> && <command>`.
/// - Never use bash for `find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, `echo`.
/// - Multiple commands: parallel tool calls for independent; `&&` for sequential dependent; `;` for sequential independent.
#[tool]
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

        // Merge stdout and stderr into a single stream
        let mut output = String::new();

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

                if !output.is_empty() {
                    yield Ok(output.clone());
                }

                yield Ok(format_metadata("null", &description, Some(&e)));
                return;
            }
        };

        // Yield the output
        if !output.is_empty() {
            yield Ok(output);
        }

        // Yield metadata
        let exit_str = match exit_code {
            Some(code) => code.to_string(),
            None => "null".to_string(),
        };
        yield Ok(format_metadata(&exit_str, &description, None));
    }
}

/// Read stdout and stderr from a child process, merging into a single output string.
async fn read_process_output(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    output: &mut String,
) -> Result<()> {
    use tokio_stream::StreamExt;

    fn lines_stream<R: tokio::io::AsyncBufRead + Unpin + Send>(
        reader: R,
    ) -> impl tokio_stream::Stream<Item = Result<String>> + Send {
        tokio_stream::wrappers::LinesStream::new(reader.lines())
            .map(|l| l.map_err(anyhow::Error::from))
    }

    let stdout_stream = stdout.map(|s| lines_stream(BufReader::new(s)));
    let stderr_stream = stderr.map(|s| lines_stream(BufReader::new(s)));

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
            let line = line?;
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&line);
        }
    }

    Ok(())
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
