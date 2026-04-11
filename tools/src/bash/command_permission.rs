use std::path::Path;

use anyhow::Result;
use llm_rs::permission::{KEY_COMMAND, SCOPE_BASH, ScopedPermissionManager, WILDCARD_VALUE};

use super::command_parser::{CommandClassification, parse_command, try_decompose_complex};
use crate::file_permission::{check_file_read_permission, check_file_write_permission};

/// Check bash command permissions using a four-layer system.
///
/// Layer 1: Read-only commands → file read permission per path
/// Layer 2: Constructive-write commands → file write permission per path
/// Layer 3: Other simple commands → hierarchical command prefix permission
/// Layer 4: Complex commands → recursively decompose, or prompt as last resort
///
/// The `bash/command/*` wildcard does NOT bypass `file_read` / `file_write`
/// defenses for classified read/write sub-commands. The wildcard only short-
/// circuits the `OtherSimple` branch (transparently via `has_permission_for`)
/// and the non-decomposable `Complex` branch (the only place we can't see what
/// the command will actually do).
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

    // Top-level redirect file permissions always enforced — wildcard never
    // bypasses these.
    for path in &parsed.redirections.input_files {
        check_file_read_permission(permission, path, false).await?;
    }
    for path in &parsed.redirections.output_files {
        check_file_write_permission(permission, path, command, "bash").await?;
    }

    match &parsed.classification {
        // Layer 4: complex → try decomposition first; if decomposable, recurse
        // into each sub-command so file_read/file_write defenses fire on
        // ReadCommand/WriteCommand sub-commands. Only fully opaque commands
        // (eval, command substitution, subshells, expansions) can be auto-
        // approved by the wildcard.
        CommandClassification::Complex => {
            if let Some(decomposed) = try_decompose_complex(command) {
                // Compound-level redirects (e.g., `cmd1 | cmd2 > file`).
                for path in &decomposed.redirections.input_files {
                    check_file_read_permission(permission, path, false).await?;
                }
                for path in &decomposed.redirections.output_files {
                    check_file_write_permission(permission, path, command, "bash").await?;
                }
                // Recurse into each sub-command. Each sub-command is a strict
                // substring of the original (at least one separator consumed),
                // so recursion is bounded.
                for sub_cmd in &decomposed.sub_commands {
                    Box::pin(check_bash_permission(permission, sub_cmd, workdir)).await?;
                }
                return Ok(());
            }
            // Non-decomposable complex command (eval, command substitution,
            // subshell, process substitution, variable expansion). We can't
            // see inside, so the wildcard is the only blanket escape hatch.
            if permission.has_permission_for(SCOPE_BASH, KEY_COMMAND, WILDCARD_VALUE) {
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
        // Layer 3: other simple commands → hierarchical command prefix permission.
        // `has_command_permission` → `has_permission_for` is wildcard-aware,
        // so this branch transparently covers the `bash/command/*` case.
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
        if permission.has_permission_for(SCOPE_BASH, KEY_COMMAND, &prefix) {
            return true;
        }
    }
    false
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
        // NOTE: `default_value` must be a real command token prefix, never
        // the literal "*". "*" is reserved as a wildcard in the permission
        // store and only enters storage via the add-permission UI.
        .ask_permission_with_preview(
            SCOPE_BASH,
            &prompt,
            KEY_COMMAND,
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
        // NOTE: `full_command` must be a real command string, never the
        // literal "*". "*" is reserved as a wildcard in the permission store
        // and only enters storage via the add-permission UI.
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
