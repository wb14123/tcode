use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

use crate::session::Session;

pub struct DisplayClient {
    session: Session,
    lua_path: PathBuf,
}

impl DisplayClient {
    pub fn new(session: Session, lua_path: PathBuf) -> Self {
        Self { session, lua_path }
    }

    pub async fn run(&self) -> Result<()> {
        let display_file = self.session.display_file();
        let status_file = self.session.status_file();

        // Save terminal settings before neovim takes over
        let saved_termios = nix::sys::termios::tcgetattr(std::io::stdin()).ok();

        // Spawn neovim
        let mut nvim = spawn_nvim(&self.lua_path, &display_file, &status_file)?;
        nvim.wait().await?;

        // Restore terminal settings as a safety net
        if let Some(ref t) = saved_termios {
            nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, t)
                .context("Failed to restore terminal settings")?;
        }

        Ok(())
    }
}

fn spawn_nvim(lua_path: &PathBuf, display_file: &PathBuf, status_file: &PathBuf) -> Result<Child> {
    let lua_cmd = format!(
        "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_display('{}', '{}')",
        lua_path.display(),
        display_file.display(),
        status_file.display()
    );

    let child = Command::new("nvim")
        .args(["-c", &lua_cmd])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to spawn 'nvim' for display - is neovim installed and in PATH?")?;

    Ok(child)
}
