//! Helper to redirect stdout/stderr to prevent injected output (like from proxychains4)
//! from corrupting TUI displays.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;

/// Saved original stdin/stdout/stderr fds before redirection.
/// Using OwnedFd for safe ownership semantics.
static SAVED_FDS: OnceLock<(OwnedFd, OwnedFd, OwnedFd)> = OnceLock::new();

/// Duplicate a file descriptor safely, returning an OwnedFd.
fn dup_fd(fd: BorrowedFd<'_>) -> Option<OwnedFd> {
    let raw = nix::unistd::dup(fd.as_raw_fd()).ok()?;
    // SAFETY: nix::unistd::dup returns a new, valid fd that we now own
    Some(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Redirect stdout and stderr to log files.
/// Saves original fds for later use. Returns original stdout fd for immediate use.
pub fn redirect_output_to_files(stdout_log: &Path, stderr_log: &Path) -> Option<OwnedFd> {
    // Save original stdin/stdout/stderr fds for later use
    let original_stdin = dup_fd(std::io::stdin().as_fd())?;
    let original_stdout = dup_fd(std::io::stdout().as_fd())?;
    let original_stderr = dup_fd(std::io::stderr().as_fd())?;

    // Dup stdout again for immediate return (before we move originals into SAVED_FDS)
    let stdout_for_return = dup_fd(original_stdout.as_fd())?;

    SAVED_FDS
        .set((original_stdin, original_stdout, original_stderr))
        .ok();

    // Open log files
    let stdout_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(stdout_log)
        .ok()?;
    let stderr_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log)
        .ok()?;

    // Redirect stdout (fd 1) to the log file
    nix::unistd::dup2(stdout_file.as_raw_fd(), 1).ok()?;
    // Redirect stderr (fd 2) to the log file
    nix::unistd::dup2(stderr_file.as_raw_fd(), 2).ok()?;

    Some(stdout_for_return)
}

/// Get Stdio using the saved original terminal fds (before redirection).
pub fn get_original_stdio() -> Option<(Stdio, Stdio, Stdio)> {
    let (stdin_fd, stdout_fd, stderr_fd) = SAVED_FDS.get()?;

    let stdin = Stdio::from(dup_fd(stdin_fd.as_fd())?);
    let stdout = Stdio::from(dup_fd(stdout_fd.as_fd())?);
    let stderr = Stdio::from(dup_fd(stderr_fd.as_fd())?);

    Some((stdin, stdout, stderr))
}

/// Write a message directly to the terminal, bypassing any stdout redirection.
pub fn write_to_terminal(original_stdout: Option<OwnedFd>, msg: &str) {
    if let Some(fd) = original_stdout {
        let mut file = File::from(fd);
        if let Err(e) = write!(file, "{}", msg) {
            eprintln!("failed to write to original stdout: {e}");
        }
        // file is dropped here, closing the fd - that's fine since we got a dup
    } else if let Ok(mut tty) = File::create("/dev/tty")
        && let Err(e) = write!(tty, "{}", msg)
    {
        eprintln!("failed to write to /dev/tty: {e}");
    }
}

/// Get Stdio for spawning neovim.
pub fn get_tty_stdio() -> (Stdio, Stdio, Stdio) {
    (Stdio::inherit(), Stdio::inherit(), Stdio::inherit())
}

/// Write an error message to the original terminal stderr (before redirection).
/// Falls back to /dev/tty if original fds aren't available.
pub fn write_error_to_terminal(msg: &str) {
    if let Some((_, _, stderr_fd)) = SAVED_FDS.get()
        && let Some(fd) = dup_fd(stderr_fd.as_fd())
    {
        let mut file = File::from(fd);
        if let Err(e) = writeln!(file, "{}", msg) {
            eprintln!("failed to write to original stderr: {e}");
        }
        return;
    }
    // Fallback to /dev/tty
    if let Ok(mut tty) = File::create("/dev/tty")
        && let Err(e) = writeln!(tty, "{}", msg)
    {
        eprintln!("failed to write to /dev/tty: {e}");
    }
}
