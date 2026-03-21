use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

use crate::session::Session;
use crate::tty_stdio;

pub struct ToolCallDisplayClient {
    session: Session,
    lua_path: PathBuf,
    tool_call_id: String,
}

impl ToolCallDisplayClient {
    pub fn new(session: Session, lua_path: PathBuf, tool_call_id: String) -> Self {
        Self {
            session,
            lua_path,
            tool_call_id,
        }
    }

    pub async fn run(&self) -> Result<()> {
        let tool_call_file = self.session.tool_call_file(&self.tool_call_id);
        let status_file = self.session.tool_call_status_file(&self.tool_call_id);

        // Create the JSONL file if it doesn't exist yet (the tool call may not have started)
        if !tool_call_file.exists() {
            tokio::fs::write(&tool_call_file, "")
                .await
                .with_context(|| format!("Failed to create tool call file {:?}", tool_call_file))?;
        }
        // Create the status file if it doesn't exist yet
        if !status_file.exists() {
            tokio::fs::write(&status_file, "Waiting")
                .await
                .with_context(|| {
                    format!("Failed to create tool call status file {:?}", status_file)
                })?;
        }

        // Save terminal settings before neovim takes over
        let saved_termios = nix::sys::termios::tcgetattr(std::io::stdin()).ok();

        // Spawn neovim for tool call display
        let mut nvim = spawn_nvim(&self.lua_path, &tool_call_file, &status_file)?;
        nvim.wait().await?;

        // Restore terminal settings as a safety net
        if let Some(ref t) = saved_termios {
            nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, t)
                .context("Failed to restore terminal settings")?;
        }

        Ok(())
    }
}

fn spawn_nvim(lua_path: &Path, tool_call_file: &Path, status_file: &Path) -> Result<Child> {
    let lua_cmd = format!(
        "lua package.path = '{}' .. '/?.lua;' .. package.path; require('tcode').setup_tool_call_display('{}', '{}')",
        lua_path.display(),
        tool_call_file.display(),
        status_file.display()
    );

    let (stdin, stdout, stderr) = tty_stdio::get_tty_stdio();

    let child = Command::new("nvim")
        .args(["-c", &lua_cmd])
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context(
            "Failed to spawn 'nvim' for tool call display - is neovim installed and in PATH?",
        )?;

    Ok(child)
}
