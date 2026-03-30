use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

use crate::session::Session;
use crate::tty_stdio;

pub struct DisplayClient {
    session: Session,
    lua_dir: PathBuf,
    session_id: String,
}

impl DisplayClient {
    pub fn new(session: Session, lua_dir: PathBuf, session_id: String) -> Self {
        Self {
            session,
            lua_dir,
            session_id,
        }
    }

    pub async fn run(&self) -> Result<()> {
        let display_file = self.session.display_file();
        let status_file = self.session.status_file();
        let usage_file = self.session.usage_file();
        let exe_path =
            std::env::current_exe().context("Failed to determine current executable path")?;

        // Save terminal settings before neovim takes over
        let saved_termios = nix::sys::termios::tcgetattr(std::io::stdin()).ok();

        // Spawn neovim
        let mut nvim = spawn_nvim(
            &self.lua_dir,
            &display_file,
            &status_file,
            &usage_file,
            &self.session_id,
            &exe_path,
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

fn spawn_nvim(
    lua_dir: &Path,
    display_file: &Path,
    status_file: &Path,
    usage_file: &Path,
    session_id: &str,
    exe_path: &Path,
) -> Result<Child> {
    let lua_cmd = format!(
        "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_display('{}', '{}', '{}', '{}', '{}')",
        lua_dir.display(),
        display_file.display(),
        status_file.display(),
        usage_file.display(),
        session_id,
        exe_path.display(),
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
