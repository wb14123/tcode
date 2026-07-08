use std::path::Path;

use anyhow::Result;
use llm_rs::permission::{KEY_COMMAND, SCOPE_BASH, ScopedPermissionManager, WILDCARD_VALUE};

use super::command_parser::{CommandClassification, parse_command, try_decompose_complex};
use crate::file_permission::{check_file_read_permission, check_file_write_permission};

fn is_under_project_root(workdir: &Path, project_root: &Path) -> bool {
    match (
        std::fs::canonicalize(workdir),
        std::fs::canonicalize(project_root),
    ) {
        (Ok(w), Ok(p)) => w.starts_with(&p),
        // If canonicalization fails, be conservative: treat as outside project.
        _ => false,
    }
}

/// Check bash command permissions using a four-layer system.
///
/// Layer 1: Read-only commands → file read permission per path
/// Layer 2: Constructive-write commands → file write permission per path
/// Layer 3: Other simple commands → hierarchical command prefix permission
/// Layer 4: Complex commands → recursively decompose, or prompt as last resort
///
/// The `bash/command/*` wildcard grants full trust — all commands auto-approve
/// regardless of workdir, file paths, or complexity. This is checked first,
/// before any redirect validation or layer classification.
///
/// `workdir` is always the concrete working directory (resolved from user input
/// or defaulted to current_dir). `project_root` is the project's current directory.
pub async fn check_bash_permission(
    permission: &ScopedPermissionManager,
    command: &str,
    workdir: &Path,
    project_root: &Path,
) -> Result<()> {
    // Wildcard = full trust. Skip everything.
    if permission.has_permission_for(SCOPE_BASH, KEY_COMMAND, WILDCARD_VALUE) {
        return Ok(());
    }

    // Workdir outside project root? Prompt once, before any other checks.
    // This gates all downstream checks (redirects, classification, file
    // permissions) behind a single prompt, avoiding cascading prompts when
    // a complex command decomposes into many sub-commands.
    if !is_under_project_root(workdir, project_root) {
        return prompt_outside_project_permission(permission, command, workdir).await;
    }

    let parsed = parse_command(command);

    // Top-level redirect file permissions — only reachable when no wildcard
    // is stored (wildcard has already short-circuited at the top).
    for path in &parsed.redirections.input_files {
        check_file_read_permission(permission, path, false).await?;
    }
    for path in &parsed.redirections.output_files {
        check_file_write_permission(permission, path, command, "bash").await?;
    }

    match &parsed.classification {
        // Layer 4: complex → try decomposition first; if decomposable, recurse
        // into each sub-command so file_read/file_write defenses fire on
        // ReadCommand/WriteCommand sub-commands. Non-decomposable opaque
        // commands (eval, command substitution, subshells, expansions) always
        // prompt — the wildcard is already checked at the top of this function.
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
                    Box::pin(check_bash_permission(
                        permission,
                        sub_cmd,
                        workdir,
                        project_root,
                    ))
                    .await?;
                }
                return Ok(());
            }
            // Non-decomposable complex command (eval, command substitution,
            // subshell, process substitution, variable expansion). We can't
            // see inside, so always prompt.
            prompt_complex_command_permission(permission, command, workdir).await
        }
        // Layer 1: read-only commands → check file read permission per path.
        CommandClassification::ReadCommand { paths } => {
            check_file_read_permission(permission, workdir, true).await?;
            for path in paths {
                check_file_read_permission(permission, path, false).await?;
            }
            Ok(())
        }
        // Layer 2: constructive-write commands → check file write permission per path.
        CommandClassification::WriteCommand { paths } => {
            check_file_write_permission(permission, workdir, command, "bash").await?;
            for path in paths {
                check_file_write_permission(permission, path, command, "bash").await?;
            }
            Ok(())
        }
        // Layer 3: other simple commands → hierarchical command prefix permission.
        // Workdir boundary is already checked at the top of this function.
        CommandClassification::OtherSimple { tokens } => {
            if has_command_permission(permission, tokens) {
                return Ok(());
            }
            prompt_command_permission(permission, command, workdir).await
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

/// Prompt the user for command permission, showing the full command as preview.
///
/// The default stored value is the command + first subcommand token,
/// which the user can edit to broaden or narrow. Tokens that look like
/// paths or flags are skipped (e.g. `find /tmp` → `"find"`, not `"find /tmp"`).
///
/// The prompt always shows the working directory since it is always concrete.
async fn prompt_command_permission(
    permission: &ScopedPermissionManager,
    full_command: &str,
    workdir: &Path,
) -> Result<()> {
    let tokens: Vec<&str> = full_command.split_whitespace().collect();
    let default_value = if tokens.len() >= 2 && looks_like_subcommand(tokens[1]) {
        format!("{} {}", tokens[0], tokens[1])
    } else if !tokens.is_empty() {
        tokens[0].to_string()
    } else {
        full_command.to_string()
    };

    let prompt = format!(
        "Allow running: `{}` in `{}`?",
        full_command,
        workdir.display()
    );

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
    workdir: &Path,
) -> Result<()> {
    let prompt = format!(
        "Allow running: `{}` in `{}`?",
        full_command,
        workdir.display()
    );

    permission
        // NOTE: `full_command` must be a real command string, never the
        // literal "*". "*" is reserved as a wildcard in the permission store
        // and only enters storage via the add-permission UI.
        .ask_permission_once(SCOPE_BASH, &prompt, full_command, "bash")
        .await
}

/// Prompt the user when the working directory is outside the project root.
/// Uses once-only prompt — no session/project caching.
async fn prompt_outside_project_permission(
    permission: &ScopedPermissionManager,
    full_command: &str,
    workdir: &Path,
) -> Result<()> {
    let prompt = format!(
        "Running `{}` outside the project directory in `{}`. Allow?",
        full_command,
        workdir.display(),
    );
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
