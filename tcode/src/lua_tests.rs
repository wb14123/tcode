//! Runs the Lua test suites for the embedded display script (tcode.lua)
//! through headless nvim.
//!
//! nvim is a hard runtime dependency of the tcode display/edit/tool-call
//! windows (see display.rs / edit.rs / tool_call_display.rs), so this test
//! FAILS with a clear message when nvim is missing rather than silently
//! skipping. Override the binary with the TCODE_NVIM environment variable.
//!
//! The suites live in `lua/tests/` next to `lua/tcode.lua` and are executed
//! by `lua/tests/runner.lua`, which loads the real tcode.lua with in-memory
//! test accessors (the production file is never modified).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

/// Per-test temp dir under the workspace target dir; removed on drop
/// (cleanup runs on success and on panic).
struct TestDir(PathBuf);

impl TestDir {
    fn new(module: &str) -> Self {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../target/test-tmp/{module}"));
        std::fs::create_dir_all(&root).expect("failed to create test root");
        let dir = root.join(uuid::Uuid::new_v4().to_string());
        // Cleanup before: remove any stale leftover at this exact path.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn tcode_lua_suites_pass() -> anyhow::Result<()> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    // nvim is a hard dependency of the product itself; fail loudly when the
    // test cannot run.
    let nvim = std::env::var("TCODE_NVIM").unwrap_or_else(|_| "nvim".to_string());
    let nvim_available = Command::new(&nvim)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    anyhow::ensure!(
        nvim_available,
        "nvim is required to run the tcode.lua test suites \
         (set TCODE_NVIM to override the binary); the tcode display/edit windows depend on it"
    );

    // Collect the Lua suite files.
    let suites_dir = manifest_dir.join("lua/tests");
    let mut suite_files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&suites_dir)
        .with_context(|| format!("reading suite dir {}", suites_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if file_name.ends_with("_tests.lua") {
            suite_files.push(path);
        }
    }
    suite_files.sort();
    anyhow::ensure!(
        !suite_files.is_empty(),
        "no *_tests.lua suites found in {}",
        suites_dir.display()
    );

    // Run the runner in headless nvim with a dedicated temp dir.
    let test_dir = TestDir::new("lua_tests");
    let mut cmd = Command::new(&nvim);
    cmd.arg("--headless")
        .arg("-l")
        .arg(manifest_dir.join("lua/tests/runner.lua"))
        .arg(manifest_dir.join("lua/tcode.lua"))
        .arg("--tmp")
        .arg(test_dir.path());
    for suite in &suite_files {
        cmd.arg(suite);
    }
    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn {nvim} --headless"))?;

    let stdout =
        String::from_utf8(output.stdout).context("nvim test runner stdout was not valid UTF-8")?;
    let stderr =
        String::from_utf8(output.stderr).context("nvim test runner stderr was not valid UTF-8")?;

    // The original bug surfaced as an uncaught scheduled-callback error; fail
    // if that exact message ever reappears.
    anyhow::ensure!(
        !stderr.contains("Buffer is not 'modifiable'"),
        "tcode.lua suites reported the modifiable error:\n{stderr}"
    );

    anyhow::ensure!(
        output.status.success(),
        "tcode.lua suites failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    // The runner always prints a TOTAL line; require it plus zero failures so
    // a mis-wired runner that executes no tests cannot pass silently.
    let total_line = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with("TOTAL: "))
        .with_context(|| format!("runner printed no TOTAL line:\n{stdout}"))?;
    anyhow::ensure!(
        total_line.ends_with("0 failed"),
        "tcode.lua suites reported failures: {total_line}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    let passed: usize = total_line
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("TOTAL line missing pass count: {total_line}"))?;
    anyhow::ensure!(
        passed > 0,
        "tcode.lua suites ran zero assertions: {total_line}"
    );

    println!("tcode.lua suites: {total_line}");
    Ok(())
}
