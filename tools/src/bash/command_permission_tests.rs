use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use llm_rs::permission::{
    KEY_COMMAND, PermissionDecision, PermissionKey, PermissionManager, PermissionScope, SCOPE_BASH,
    ScopedPermissionManager, WILDCARD_VALUE,
};

use super::command_permission::{
    check_bash_permission, command_matches_permission, has_command_permission,
};

fn test_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/test-tmp/bash_command_permission")
}

fn temp_perm_path() -> PathBuf {
    let root = test_root();
    std::fs::create_dir_all(&root).expect("failed to create test root");
    root.join(format!("perm-{}.json", uuid::Uuid::new_v4()))
}

/// Build a unique path under the workspace test-tmp directory for use in tests
/// that exercise real filesystem-touching commands. Includes a uuid so parallel
/// tests don't collide.
fn unique_temp_path(name: &str) -> PathBuf {
    let root = test_root();
    std::fs::create_dir_all(&root).expect("failed to create test root");
    root.join(format!("{}-{}", name, uuid::Uuid::new_v4()))
}

/// Returns a concrete workdir + project_root pair where workdir is under
/// project_root. Uses the workspace directory as the project root.
fn in_project_workdir() -> (PathBuf, PathBuf) {
    let project_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/bash_cmd_perm");
    std::fs::create_dir_all(&project_root).expect("failed to create project root");
    let workdir = project_root.clone();
    (workdir, project_root)
}

/// Returns a concrete workdir + project_root pair where workdir is OUTSIDE
/// project_root.
fn outside_project_workdir() -> (PathBuf, PathBuf) {
    let project_root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/bash_cmd_perm");
    std::fs::create_dir_all(&project_root).expect("failed to create project root");
    // /tmp is outside the project root
    let workdir = std::path::PathBuf::from("/tmp");
    (workdir, project_root)
}

/// Poll `pm.snapshot().pending.len()` until it equals `expected`, returning Err on
/// timeout. Used by tests that spawn `check_bash_permission` in a task and need to
/// wait for a prompt to register.
async fn wait_for_pending(pm: &PermissionManager, expected: usize) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let count = pm.snapshot().pending.len();
        if count == expected {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for {} pending requests (have {})",
                expected,
                count
            ));
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[test]
fn command_matches_exact() {
    assert!(command_matches_permission("git", "git"));
}

#[test]
fn command_matches_with_args() {
    assert!(command_matches_permission("git", "git diff"));
    assert!(command_matches_permission("git", "git add ."));
    assert!(command_matches_permission("git", "git push origin"));
}

#[test]
fn command_does_not_match_prefix_without_boundary() {
    assert!(!command_matches_permission("git", "gitabc"));
}

#[test]
fn subcommand_match() {
    assert!(command_matches_permission("git push", "git push"));
    assert!(command_matches_permission(
        "git push",
        "git push origin main"
    ));
}

#[test]
fn subcommand_does_not_match_different_subcommand() {
    assert!(!command_matches_permission("git push", "git add"));
}

#[test]
fn cargo_matches() {
    assert!(command_matches_permission("cargo", "cargo build"));
    assert!(command_matches_permission("cargo", "cargo test --release"));
    assert!(!command_matches_permission("cargo", "cargoabc"));
}

#[test]
fn hierarchical_lookup_most_specific_first() {
    // Create a permission manager with "git add" stored
    let pm = std::sync::Arc::new(llm_rs::permission::PermissionManager::new(temp_perm_path()));

    let key = llm_rs::permission::PermissionKey {
        tool: SCOPE_BASH.to_string(),
        key: "command".to_string(),
        value: "git add".to_string(),
    };
    pm.resolve(
        &key,
        &llm_rs::permission::PermissionDecision::AllowSession,
        None,
    )
    .expect("resolve should succeed");

    let scoped = ScopedPermissionManager::new(
        "bash",
        pm,
        std::sync::Arc::new(|| {}),
        std::sync::Arc::new(|| {}),
        None,
    );

    // "git add src/main.rs" should match via prefix "git add"
    let tokens: Vec<String> = vec![
        "git".to_string(),
        "add".to_string(),
        "src/main.rs".to_string(),
    ];
    assert!(has_command_permission(&scoped, &tokens));

    // "git push" should NOT match "git add"
    let tokens2: Vec<String> = vec!["git".to_string(), "push".to_string()];
    assert!(!has_command_permission(&scoped, &tokens2));
}

#[test]
fn hierarchical_lookup_base_command() {
    let pm = std::sync::Arc::new(llm_rs::permission::PermissionManager::new(temp_perm_path()));

    let key = llm_rs::permission::PermissionKey {
        tool: SCOPE_BASH.to_string(),
        key: "command".to_string(),
        value: "cargo".to_string(),
    };
    pm.resolve(
        &key,
        &llm_rs::permission::PermissionDecision::AllowSession,
        None,
    )
    .expect("resolve should succeed");

    let scoped = ScopedPermissionManager::new(
        "bash",
        pm,
        std::sync::Arc::new(|| {}),
        std::sync::Arc::new(|| {}),
        None,
    );

    // "cargo build" should match via base "cargo"
    let tokens: Vec<String> = vec!["cargo".to_string(), "build".to_string()];
    assert!(has_command_permission(&scoped, &tokens));

    // "cargo test --release" should also match
    let tokens2: Vec<String> = vec![
        "cargo".to_string(),
        "test".to_string(),
        "--release".to_string(),
    ];
    assert!(has_command_permission(&scoped, &tokens2));
}

#[test]
fn permission_npm_match() {
    assert!(command_matches_permission("npm", "npm install"));
    assert!(command_matches_permission("npm", "npm run build"));
    assert!(!command_matches_permission("npm", "npx create"));
}

/// Helper: build an Arc<PermissionManager> with the bash/command/* wildcard
/// pre-stored, plus a wrapping ScopedPermissionManager for the "bash" tool.
fn pm_with_wildcard() -> Result<(Arc<PermissionManager>, ScopedPermissionManager)> {
    let pm = Arc::new(PermissionManager::new(temp_perm_path()));
    pm.add_permission(
        PermissionKey {
            tool: SCOPE_BASH.to_string(),
            key: KEY_COMMAND.to_string(),
            value: WILDCARD_VALUE.to_string(),
        },
        PermissionScope::Session,
    )?;
    let scoped = ScopedPermissionManager::new(
        "bash",
        Arc::clone(&pm),
        Arc::new(|| {}),
        Arc::new(|| {}),
        None,
    );
    Ok((pm, scoped))
}

// =====================================================================
// Tests for the wildcard = full trust invariant.
// The wildcard is now checked at the very top of `check_bash_permission`
// and grants full trust — all commands auto-approve regardless of
// workdir, file paths, redirects, or complexity.
// =====================================================================

/// Wildcard + write command (mkdir) → auto-approved (full trust).
#[tokio::test]
async fn wildcard_auto_approves_write_command() -> Result<()> {
    let (_pm, scoped) = pm_with_wildcard()?;
    let path = unique_temp_path("wild-mkdir");
    let cmd = format!("mkdir {}", path.display());
    let (workdir, project_root) = in_project_workdir();

    let result = check_bash_permission(&scoped, &cmd, &workdir, &project_root).await;
    assert!(
        result.is_ok(),
        "expected wildcard to auto-approve mkdir, got {:?}",
        result
    );
    Ok(())
}

/// Wildcard + read command (cat) → auto-approved (full trust).
#[tokio::test]
async fn wildcard_auto_approves_read_command() -> Result<()> {
    let (_pm, scoped) = pm_with_wildcard()?;
    let path = unique_temp_path("wild-cat-secret");
    tokio::fs::write(&path, "secret\n").await?;
    let cmd = format!("cat {}", path.display());
    let (workdir, project_root) = in_project_workdir();

    let result = check_bash_permission(&scoped, &cmd, &workdir, &project_root).await;
    let _ = tokio::fs::remove_file(&path).await;
    assert!(
        result.is_ok(),
        "expected wildcard to auto-approve cat, got {:?}",
        result
    );
    Ok(())
}

/// Wildcard + redirect (`echo hello > /tmp/foo`) → auto-approved (full trust).
#[tokio::test]
async fn wildcard_auto_approves_redirect() -> Result<()> {
    let (_pm, scoped) = pm_with_wildcard()?;
    let path = unique_temp_path("wild-redirect-out");
    let cmd = format!("echo hello > {}", path.display());
    let (workdir, project_root) = in_project_workdir();

    let result = check_bash_permission(&scoped, &cmd, &workdir, &project_root).await;
    assert!(
        result.is_ok(),
        "expected wildcard to auto-approve redirect, got {:?}",
        result
    );
    Ok(())
}

/// Wildcard + decomposable pipeline → auto-approved.
#[tokio::test]
async fn wildcard_auto_approves_decomposable_pipeline_of_other_simple() -> Result<()> {
    let (_pm, scoped) = pm_with_wildcard()?;
    let (workdir, project_root) = in_project_workdir();
    let result = check_bash_permission(&scoped, "ls | grep foo", &workdir, &project_root).await;
    assert!(
        result.is_ok(),
        "expected pipeline to auto-approve via wildcard, got {:?}",
        result
    );
    Ok(())
}

/// Wildcard + compound with write sub-command → auto-approved (full trust).
#[tokio::test]
async fn wildcard_auto_approves_compound_with_write() -> Result<()> {
    let (_pm, scoped) = pm_with_wildcard()?;
    let path = unique_temp_path("wild-compound-mkdir");
    let cmd = format!("mkdir {} && ls", path.display());
    let (workdir, project_root) = in_project_workdir();

    let result = check_bash_permission(&scoped, &cmd, &workdir, &project_root).await;
    assert!(
        result.is_ok(),
        "expected wildcard to auto-approve compound with write, got {:?}",
        result
    );
    Ok(())
}

/// Wildcard + non-decomposable complex command → auto-approved (full trust).
#[tokio::test]
async fn non_decomposable_complex_auto_approved_with_wildcard() -> Result<()> {
    let (_pm, scoped) = pm_with_wildcard()?;
    let (workdir, project_root) = in_project_workdir();
    let result = check_bash_permission(&scoped, "echo $(whoami)", &workdir, &project_root).await;
    assert!(
        result.is_ok(),
        "expected non-decomposable complex to auto-approve via wildcard, got {:?}",
        result
    );
    Ok(())
}

/// Wildcard + any command + workdir outside project root → still auto-approved.
#[tokio::test]
async fn wildcard_auto_approves_outside_project() -> Result<()> {
    let (_pm, scoped) = pm_with_wildcard()?;
    let (workdir, project_root) = outside_project_workdir();
    let result = check_bash_permission(&scoped, "echo hello", &workdir, &project_root).await;
    assert!(
        result.is_ok(),
        "expected wildcard to auto-approve even outside project, got {:?}",
        result
    );
    Ok(())
}

// =====================================================================
// Tests for non-wildcard path: non-decomposable complex commands
// =====================================================================

/// Non-decomposable complex command WITHOUT wildcard → prompts once-only.
#[tokio::test]
async fn non_decomposable_complex_without_wildcard_prompts_once_only() -> Result<()> {
    let pm = Arc::new(PermissionManager::new(temp_perm_path()));
    let scoped = ScopedPermissionManager::new(
        "bash",
        Arc::clone(&pm),
        Arc::new(|| {}),
        Arc::new(|| {}),
        None,
    );

    let (workdir, project_root) = in_project_workdir();
    let scoped_clone = scoped.clone();
    let handle = tokio::spawn(async move {
        check_bash_permission(&scoped_clone, "echo $(whoami)", &workdir, &project_root).await
    });

    wait_for_pending(&pm, 1).await?;
    let state = pm.snapshot();
    assert_eq!(state.pending.len(), 1);
    let pending = &state.pending[0];
    assert_eq!(pending.tool, SCOPE_BASH);
    assert!(
        pending.once_only,
        "non-decomposable complex command should be once_only"
    );

    let key = PermissionKey {
        tool: pending.tool.clone(),
        key: pending.key.clone(),
        value: pending.value.clone(),
    };
    pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
    let result = handle.await?;
    assert!(result.is_err());
    Ok(())
}

// =====================================================================
// Tests for Layer 3 (OtherSimple): workdir boundary
// =====================================================================

/// OtherSimple with specific command permission + workdir inside project root → auto-approves.
#[tokio::test]
async fn other_simple_approved_in_project_auto_approves() -> Result<()> {
    let pm = Arc::new(PermissionManager::new(temp_perm_path()));
    // Pre-grant "cargo"
    pm.add_permission(
        PermissionKey {
            tool: SCOPE_BASH.to_string(),
            key: KEY_COMMAND.to_string(),
            value: "cargo".to_string(),
        },
        PermissionScope::Session,
    )?;
    let scoped = ScopedPermissionManager::new(
        "bash",
        Arc::clone(&pm),
        Arc::new(|| {}),
        Arc::new(|| {}),
        None,
    );

    let (workdir, project_root) = in_project_workdir();
    let result = check_bash_permission(&scoped, "cargo build", &workdir, &project_root).await;
    assert!(
        result.is_ok(),
        "expected approved command inside project to auto-approve, got {:?}",
        result
    );
    assert!(
        pm.snapshot().pending.is_empty(),
        "expected no prompts for in-project approved command"
    );
    Ok(())
}

/// OtherSimple with specific command permission + workdir outside project root → prompts once-only.
#[tokio::test]
async fn other_simple_approved_outside_project_prompts_once_only() -> Result<()> {
    let pm = Arc::new(PermissionManager::new(temp_perm_path()));
    // Pre-grant "cargo"
    pm.add_permission(
        PermissionKey {
            tool: SCOPE_BASH.to_string(),
            key: KEY_COMMAND.to_string(),
            value: "cargo".to_string(),
        },
        PermissionScope::Session,
    )?;
    let scoped = ScopedPermissionManager::new(
        "bash",
        Arc::clone(&pm),
        Arc::new(|| {}),
        Arc::new(|| {}),
        None,
    );

    let (workdir, project_root) = outside_project_workdir();
    let scoped_clone = scoped.clone();
    let handle = tokio::spawn(async move {
        check_bash_permission(&scoped_clone, "cargo build", &workdir, &project_root).await
    });

    wait_for_pending(&pm, 1).await?;
    let state = pm.snapshot();
    assert_eq!(state.pending.len(), 1);
    let pending = &state.pending[0];
    assert_eq!(pending.tool, SCOPE_BASH);
    assert!(
        pending.once_only,
        "outside-project prompt should be once_only"
    );
    // Verify the prompt mentions "outside the project directory"
    assert!(
        pending.prompt.contains("outside the project directory"),
        "prompt should mention outside project, got: {}",
        pending.prompt
    );

    // Deny
    let key = PermissionKey {
        tool: pending.tool.clone(),
        key: pending.key.clone(),
        value: pending.value.clone(),
    };
    pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
    let result = handle.await?;
    assert!(result.is_err());
    Ok(())
}

/// OtherSimple with NO permission → prompts (always shows workdir).
#[tokio::test]
async fn other_simple_no_permission_prompts_with_workdir() -> Result<()> {
    let pm = Arc::new(PermissionManager::new(temp_perm_path()));
    let scoped = ScopedPermissionManager::new(
        "bash",
        Arc::clone(&pm),
        Arc::new(|| {}),
        Arc::new(|| {}),
        None,
    );

    let (workdir, project_root) = in_project_workdir();
    let workdir_display = workdir.display().to_string();
    let scoped_clone = scoped.clone();
    let handle = tokio::spawn(async move {
        check_bash_permission(&scoped_clone, "git status", &workdir, &project_root).await
    });

    wait_for_pending(&pm, 1).await?;
    let state = pm.snapshot();
    assert_eq!(state.pending.len(), 1);
    let pending = &state.pending[0];
    assert_eq!(pending.tool, SCOPE_BASH);
    assert!(
        !pending.once_only,
        "unapproved command should use ask_permission_with_preview (not once_only)"
    );

    // Verify prompt includes workdir
    assert!(
        pending.prompt.contains(&workdir_display),
        "prompt should include workdir, got: {}",
        pending.prompt
    );

    // Deny
    let key = PermissionKey {
        tool: pending.tool.clone(),
        key: pending.key.clone(),
        value: pending.value.clone(),
    };
    pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
    let result = handle.await?;
    assert!(result.is_err());
    Ok(())
}

/// Compound with read command (without wildcard) prompts for file_read on the cat target.
#[tokio::test]
async fn compound_with_read_command_prompts_for_file_read() -> Result<()> {
    let pm = Arc::new(PermissionManager::new(temp_perm_path()));
    let scoped = ScopedPermissionManager::new(
        "bash",
        Arc::clone(&pm),
        Arc::new(|| {}),
        Arc::new(|| {}),
        None,
    );

    // The file must exist — `check_file_read_permission` errors out for
    // non-existent paths before any prompt is issued.
    let path = unique_temp_path("compound-cat");
    tokio::fs::write(&path, "secret\n").await?;
    let cmd = format!("cat {} && ls", path.display());

    let (workdir, project_root) = in_project_workdir();
    let scoped_clone = scoped.clone();
    let cmd_clone = cmd.clone();
    let handle = tokio::spawn(async move {
        check_bash_permission(&scoped_clone, &cmd_clone, &workdir, &project_root).await
    });

    let wait_result = wait_for_pending(&pm, 1).await;
    if wait_result.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
        wait_result?;
    }
    let pending = pm.snapshot().pending;
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].tool, "file_read",
        "first prompt should be file_read for cat target"
    );

    let key = PermissionKey {
        tool: pending[0].tool.clone(),
        key: pending[0].key.clone(),
        value: pending[0].value.clone(),
    };
    pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
    let result = handle.await?;
    let _ = tokio::fs::remove_file(&path).await;
    assert!(
        result.is_err(),
        "expected file_read denial to cause error, got {:?}",
        result
    );
    Ok(())
}

/// `>/dev/null` redirect auto-approves when file_write for /dev/null is granted.
#[tokio::test]
async fn dev_null_redirect_approved_with_grant() -> Result<()> {
    if !std::path::Path::new("/dev/null").exists() {
        // Non-Unix platform without /dev/null — nothing to test.
        return Ok(());
    }
    let pm = Arc::new(PermissionManager::new(temp_perm_path()));
    let canonical = tokio::fs::canonicalize("/dev/null").await?;
    pm.add_permission(
        PermissionKey {
            tool: "file_write".to_string(),
            key: "path".to_string(),
            value: canonical.to_string_lossy().to_string(),
        },
        PermissionScope::Session,
    )?;

    let (workdir, project_root) = in_project_workdir();
    // `echo hi > /dev/null` is a ReadCommand, whose layer also checks read
    // permission on the workdir itself. Grant it so the only permission under
    // test is the /dev/null redirect.
    let canonical_workdir = tokio::fs::canonicalize(&workdir).await?;
    pm.add_permission(
        PermissionKey {
            tool: "file_read".to_string(),
            key: "path".to_string(),
            value: canonical_workdir.to_string_lossy().to_string(),
        },
        PermissionScope::Session,
    )?;

    let scoped = ScopedPermissionManager::new(
        "bash",
        Arc::clone(&pm),
        Arc::new(|| {}),
        Arc::new(|| {}),
        None,
    );

    let result =
        check_bash_permission(&scoped, "echo hi > /dev/null", &workdir, &project_root).await;
    assert!(
        result.is_ok(),
        "expected /dev/null redirect to auto-approve with grant, got {:?}",
        result
    );
    assert!(
        pm.snapshot().pending.is_empty(),
        "expected no prompts for granted /dev/null redirect"
    );
    Ok(())
}

/// Without a grant, `>/dev/null` prompts for file_write on the redirect target.
#[tokio::test]
async fn dev_null_redirect_prompts_without_grant() -> Result<()> {
    if !std::path::Path::new("/dev/null").exists() {
        // Non-Unix platform without /dev/null — nothing to test.
        return Ok(());
    }
    let pm = Arc::new(PermissionManager::new(temp_perm_path()));
    let scoped = ScopedPermissionManager::new(
        "bash",
        Arc::clone(&pm),
        Arc::new(|| {}),
        Arc::new(|| {}),
        None,
    );

    let (workdir, project_root) = in_project_workdir();
    let scoped_clone = scoped.clone();
    let handle = tokio::spawn(async move {
        check_bash_permission(
            &scoped_clone,
            "echo hi > /dev/null",
            &workdir,
            &project_root,
        )
        .await
    });

    wait_for_pending(&pm, 1).await?;
    let state = pm.snapshot();
    assert_eq!(state.pending.len(), 1);
    assert_eq!(
        state.pending[0].tool, "file_write",
        "redirect to /dev/null should prompt for file_write without a grant"
    );

    let key = PermissionKey {
        tool: state.pending[0].tool.clone(),
        key: state.pending[0].key.clone(),
        value: state.pending[0].value.clone(),
    };
    pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
    let result = handle.await?;
    assert!(
        result.is_err(),
        "expected file_write denial for /dev/null redirect to cause error, got {:?}",
        result
    );
    Ok(())
}
