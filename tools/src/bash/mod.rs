pub mod command_parser;
pub mod command_permission;

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

/// Executes a given bash command with optional timeout, ensuring proper handling and security measures.
///
/// IMPORTANT: This tool is for terminal operations like git, npm, docker, etc.
/// DO NOT use it for file operations (reading, writing, editing, searching, finding files)
/// - use the specialized tools for this instead.
///
/// Before executing the command, please follow these steps:
///
/// 1. Directory Verification:
///    - If the command will create new directories or files, first use `ls` to verify
///      the parent directory exists and is the correct location
///
/// 2. Command Execution:
///    - Always quote file paths that contain spaces with double quotes
///    - After ensuring proper quoting, execute the command.
///    - Capture the output of the command.
///
/// Usage notes:
///   - The command argument is required.
///   - You can specify an optional timeout in milliseconds. If not specified, commands
///     will time out after 120000ms (2 minutes).
///   - It is very helpful if you write a clear, concise description of what this command
///     does in 5-10 words.
///   - AVOID using `cd <directory> && <command>`. Use the `workdir` parameter instead.
///   - Avoid using Bash with `find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, or
///     `echo` — use dedicated tools instead.
///   - When issuing multiple commands: use parallel tool calls for independent commands,
///     '&&' for sequential dependent commands, ';' for sequential independent commands.
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

                let exit_str = "null";
                let metadata = format!(
                    "\n<bash_metadata>\nexit_code: {}\ndescription: {}\nerror: {}\n</bash_metadata>",
                    exit_str, description, e
                );
                yield Ok(metadata);
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
        let metadata = format!(
            "\n<bash_metadata>\nexit_code: {}\ndescription: {}\n</bash_metadata>",
            exit_str, description
        );
        yield Ok(metadata);
    }
}

/// Read stdout and stderr from a child process, merging into a single output string.
async fn read_process_output(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    output: &mut String,
) -> Result<()> {
    use tokio_stream::{StreamExt, wrappers::LinesStream};

    let stdout_stream = stdout.map(|s| {
        LinesStream::new(BufReader::new(s).lines()).map(|l| l.map_err(anyhow::Error::from))
    });
    let stderr_stream = stderr.map(|s| {
        LinesStream::new(BufReader::new(s).lines()).map(|l| l.map_err(anyhow::Error::from))
    });

    // Merge both streams to preserve real interleaving order of stdout/stderr
    match (stdout_stream, stderr_stream) {
        (Some(out), Some(err)) => {
            let mut merged = StreamExt::merge(out, err);
            while let Some(line) = merged.next().await {
                let line = line?;
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&line);
            }
        }
        (Some(mut stream), None) => {
            while let Some(line) = stream.next().await {
                let line = line?;
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&line);
            }
        }
        (None, Some(mut stream)) => {
            while let Some(line) = stream.next().await {
                let line = line?;
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&line);
            }
        }
        (None, None) => {}
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

    // Give the process group 2 seconds to exit on its own
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

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
