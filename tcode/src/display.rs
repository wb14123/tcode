use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

use crate::lua_escape;
use crate::session::Session;
use crate::tty_stdio;

pub struct DisplayClient {
    session: Session,
    lua_dir: PathBuf,
    session_id: String,
    /// Root directory prepended to Neovim's runtimepath so that
    /// `queries/tcode/{injections,highlights}.scm` are discovered by tree-sitter.
    runtime_dir: PathBuf,
}

/// Find the tree-sitter tcode parser library. Checks (in order):
/// 1. Next to the executable (dev builds: `target/debug/`)
/// 2. `../lib` relative to exe (installed: `/usr/local/bin` → `/usr/local/lib`)
fn parser_lib_path(exe_path: &Path) -> PathBuf {
    let name = if cfg!(target_os = "macos") {
        "libtree-sitter-tcode.dylib"
    } else {
        "libtree-sitter-tcode.so"
    };
    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));
    // Check next to executable first (dev builds)
    let beside_exe = exe_dir.join(name);
    if beside_exe.exists() {
        return beside_exe;
    }
    // Check ../lib (e.g. /usr/local/bin/tcode → /usr/local/lib/)
    let lib_dir = exe_dir.join("../lib").join(name);
    if lib_dir.exists() {
        return lib_dir;
    }
    // Fall back to next to exe (will trigger the pcall error gracefully)
    beside_exe
}

impl DisplayClient {
    pub fn new(
        session: Session,
        lua_dir: PathBuf,
        session_id: String,
        runtime_dir: PathBuf,
    ) -> Self {
        Self {
            session,
            lua_dir,
            session_id,
            runtime_dir,
        }
    }

    pub async fn run(&self) -> Result<()> {
        let display_file = self.session.display_file();
        let status_file = self.session.status_file();
        let usage_file = self.session.usage_file();
        let token_usage_file = self.session.token_usage_file();
        let exe_path =
            std::env::current_exe().context("Failed to determine current executable path")?;
        let parser_path = parser_lib_path(&exe_path);

        // Pre-create usage and token_usage files so the nvim fs_event watchers can
        // attach immediately instead of retrying. Without this, the watchers give up
        // after ~20 seconds of retries, so if the first assistant response takes longer
        // (or usage fetch fails with e.g. 429), the status bar never updates.
        for path in [&usage_file, &token_usage_file] {
            if !path.exists() {
                tokio::fs::write(path, "")
                    .await
                    .with_context(|| format!("Failed to pre-create {:?}", path))?;
            }
        }

        // Save terminal settings before neovim takes over
        let saved_termios = nix::sys::termios::tcgetattr(std::io::stdin()).ok();

        // Spawn neovim
        let mut nvim = spawn_nvim(
            &self.lua_dir,
            &display_file,
            &status_file,
            &usage_file,
            &token_usage_file,
            &self.session_id,
            &exe_path,
            &parser_path,
            &self.runtime_dir,
        )?;
        nvim.wait().await?;

        // Restore terminal settings as a safety net
        if let Some(ref t) = saved_termios {
            nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, t)
                .context("Failed to restore terminal settings")?;
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_nvim(
    lua_dir: &Path,
    display_file: &Path,
    status_file: &Path,
    usage_file: &Path,
    token_usage_file: &Path,
    session_id: &str,
    exe_path: &Path,
    parser_path: &Path,
    runtime_dir: &Path,
) -> Result<Child> {
    let lua_cmd = format!(
        "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_display('{}', '{}', '{}', '{}', '{}', '{}', '{}', '{}')",
        lua_escape(&lua_dir.display().to_string()),
        lua_escape(&display_file.display().to_string()),
        lua_escape(&status_file.display().to_string()),
        lua_escape(&usage_file.display().to_string()),
        lua_escape(&token_usage_file.display().to_string()),
        lua_escape(session_id),
        lua_escape(&exe_path.display().to_string()),
        lua_escape(&parser_path.display().to_string()),
        lua_escape(&runtime_dir.display().to_string()),
    );

    let (stdin, stdout, stderr) = tty_stdio::get_tty_stdio();

    let child = Command::new("nvim")
        .args(["-c", &lua_cmd])
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("Failed to spawn 'nvim' for display - is neovim installed and in PATH?")?;

    Ok(child)
}
