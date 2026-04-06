use std::path::Path;

use anyhow::Result;
use llm_rs::permission::{SCOPE_BASH, ScopedPermissionManager};

use super::command_parser::{CommandClassification, parse_command, try_decompose_complex};
use crate::file_permission::{
    check_file_read_permission, check_file_write_permission, has_file_read_permission,
    has_file_write_permission,
};

/// Check bash command permissions using a four-layer system.
///
/// Layer 1: Read-only commands → file read permission per path
/// Layer 2: Constructive-write commands → file write permission per path
/// Layer 3: Other simple commands → hierarchical command prefix permission
/// Layer 4: Complex commands → always prompt via command permission
///
/// If `workdir` is provided, it is included in the paths checked for
/// file permission — read permission for read commands, write permission
/// for write commands.
pub async fn check_bash_permission(
    permission: &ScopedPermissionManager,
    command: &str,
    workdir: Option<&Path>,
) -> Result<()> {
    let parsed = parse_command(command);

    // Always check redirect file permissions regardless of classification
    for path in &parsed.redirections.input_files {
        check_file_read_permission(permission, path, false).await?;
    }
    for path in &parsed.redirections.output_files {
        check_file_write_permission(permission, path, command, "bash").await?;
    }

    match &parsed.classification {
        // Layer 4: complex → try decomposition fast-path, otherwise always prompt
        CommandClassification::Complex => {
            if try_decomposed_permission(permission, command, workdir).await {
                return Ok(());
            }
            prompt_complex_command_permission(permission, command, workdir).await
        }
        // Layer 1: read-only commands → check file read permission per path
        CommandClassification::ReadCommand { paths } => {
            if let Some(dir) = workdir {
                check_file_read_permission(permission, dir, true).await?;
            }
            for path in paths {
                check_file_read_permission(permission, path, false).await?;
            }
            Ok(())
        }
        // Layer 2: constructive-write commands → check file write permission per path
        CommandClassification::WriteCommand { paths } => {
            if let Some(dir) = workdir {
                check_file_write_permission(permission, dir, command, "bash").await?;
            }
            for path in paths {
                check_file_write_permission(permission, path, command, "bash").await?;
            }
            Ok(())
        }
        // Layer 3: other simple commands → hierarchical command prefix permission
        CommandClassification::OtherSimple { tokens } => {
            check_command_permission(permission, tokens, command, workdir).await
        }
    }
}

/// Check if a stored command permission prefix matches the given command tokens.
///
/// Walks from most-specific to least-specific prefix (mirrors `has_ancestor_permission`
/// in file_permission.rs — here we walk up the command prefix tree).
pub(crate) fn has_command_permission(
    permission: &ScopedPermissionManager,
    tokens: &[String],
) -> bool {
    for i in (1..=tokens.len()).rev() {
        let prefix = tokens[..i].join(" ");
        if permission.has_permission_for(SCOPE_BASH, "command", &prefix) {
            return true;
        }
    }
    false
}

/// Attempt to auto-approve a complex command by decomposing it into sub-commands
/// and checking if ALL parts already have stored permissions.
///
/// Returns `true` if every sub-command (and any top-level redirections) is already
/// approved — the complex command can proceed without prompting.
/// Returns `false` if decomposition fails or any part lacks a stored permission —
/// the caller should fall back to prompting for the whole complex command.
async fn try_decomposed_permission(
    permission: &ScopedPermissionManager,
    command: &str,
    workdir: Option<&Path>,
) -> bool {
    let decomposed = match try_decompose_complex(command) {
        Some(d) => d,
        None => return false,
    };

    // Check top-level redirections (e.g., pipeline-level `> file`)
    for path in &decomposed.redirections.input_files {
        if !has_file_read_permission(permission, path, false).await {
            return false;
        }
    }
    for path in &decomposed.redirections.output_files {
        if !has_file_write_permission(permission, path).await {
            return false;
        }
    }

    // Check each sub-command using the same logic as the non-Complex branches
    for sub_cmd in &decomposed.sub_commands {
        if !has_simple_command_permission(permission, sub_cmd, workdir).await {
            return false;
        }
    }

    true
}

/// Check if a single (non-Complex) sub-command already has stored permission,
/// without prompting. Returns `true` if approved, `false` if not.
///
/// This mirrors the non-Complex branches of `check_bash_permission` but
/// only checks — never prompts.
async fn has_simple_command_permission(
    permission: &ScopedPermissionManager,
    sub_command: &str,
    workdir: Option<&Path>,
) -> bool {
    let parsed = parse_command(sub_command);

    // Check redirect file permissions for this sub-command
    for path in &parsed.redirections.input_files {
        if !has_file_read_permission(permission, path, false).await {
            return false;
        }
    }
    for path in &parsed.redirections.output_files {
        if !has_file_write_permission(permission, path).await {
            return false;
        }
    }

    match &parsed.classification {
        CommandClassification::Complex => {
            // Should not happen — try_decompose_complex already verified all
            // sub-commands parse as non-Complex. But if it does, fail safely.
            false
        }
        CommandClassification::ReadCommand { paths } => {
            if let Some(dir) = workdir
                && !has_file_read_permission(permission, dir, true).await
            {
                return false;
            }
            for path in paths {
                if !has_file_read_permission(permission, path, false).await {
                    return false;
                }
            }
            true
        }
        CommandClassification::WriteCommand { paths } => {
            if let Some(dir) = workdir
                && !has_file_write_permission(permission, dir).await
            {
                return false;
            }
            for path in paths {
                if !has_file_write_permission(permission, path).await {
                    return false;
                }
            }
            true
        }
        CommandClassification::OtherSimple { tokens } => has_command_permission(permission, tokens),
    }
}

/// Check command permission using hierarchical prefix matching.
/// If no existing permission matches, prompt the user.
async fn check_command_permission(
    permission: &ScopedPermissionManager,
    tokens: &[String],
    full_command: &str,
    workdir: Option<&Path>,
) -> Result<()> {
    if has_command_permission(permission, tokens) {
        return Ok(());
    }
    prompt_command_permission(permission, full_command, workdir).await
}

/// Prompt the user for command permission, showing the full command as preview.
///
/// The default stored value is the command + first subcommand token,
/// which the user can edit to broaden or narrow. Tokens that look like
/// paths or flags are skipped (e.g. `find /tmp` → `"find"`, not `"find /tmp"`).
///
/// If `workdir` is provided, the prompt includes the working directory
/// so the user can see where the command will run.
async fn prompt_command_permission(
    permission: &ScopedPermissionManager,
    full_command: &str,
    workdir: Option<&Path>,
) -> Result<()> {
    let tokens: Vec<&str> = full_command.split_whitespace().collect();
    let default_value = if tokens.len() >= 2 && looks_like_subcommand(tokens[1]) {
        format!("{} {}", tokens[0], tokens[1])
    } else if !tokens.is_empty() {
        tokens[0].to_string()
    } else {
        full_command.to_string()
    };

    let prompt = match workdir {
        Some(dir) => format!("Allow running: `{}` in `{}`?", full_command, dir.display()),
        None => format!("Allow running: `{}`?", full_command),
    };

    permission
        .ask_permission_with_preview(
            SCOPE_BASH,
            &prompt,
            "command",
            &default_value,
            full_command,
            "bash",
        )
        .await
}

/// Prompt the user for a complex command. Always prompts (no cache lookup)
/// and only offers "Allow once" / "Deny" — no session/project caching.
async fn prompt_complex_command_permission(
    permission: &ScopedPermissionManager,
    full_command: &str,
    workdir: Option<&Path>,
) -> Result<()> {
    let prompt = match workdir {
        Some(dir) => format!("Allow running: `{}` in `{}`?", full_command, dir.display()),
        None => format!("Allow running: `{}`?", full_command),
    };

    permission
        .ask_permission_once(SCOPE_BASH, &prompt, full_command, "bash")
        .await
}

/// A token looks like a subcommand (e.g. "add", "build", "run") rather than
/// a path or flag argument.
fn looks_like_subcommand(token: &str) -> bool {
    !token.starts_with('-') && !token.starts_with('.') && !token.contains('/')
}

/// Check if a stored permission value matches an actual command string.
/// Word-boundary aware: the stored prefix must match either the full
/// command or be followed by a space.
#[cfg(test)]
pub(crate) fn command_matches_permission(permission_value: &str, actual_command: &str) -> bool {
    actual_command == permission_value
        || actual_command.starts_with(&format!("{} ", permission_value))
}
