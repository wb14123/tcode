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
// Tests for the post-refactor invariant: `bash/command/*` does NOT
// bypass file_read / file_write defenses for classified read/write
// sub-commands or for top-level redirects. The wildcard only auto-
// approves the OtherSimple branch (transparently via `has_permission_for`)
// and the non-decomposable Complex branch.
// =====================================================================

/// Test A — a standalone `mkdir` (classified WriteCommand) must prompt for
/// `file_write` even when the bash wildcard is stored.
#[tokio::test]
async fn wildcard_does_not_bypass_file_write_for_write_command() -> Result<()> {
    let (pm, scoped) = pm_with_wildcard()?;
    let path = unique_temp_path("wild-mkdir");
    let cmd = format!("mkdir {}", path.display());

    let scoped_clone = scoped.clone();
    let cmd_clone = cmd.clone();
    let handle =
        tokio::spawn(async move { check_bash_permission(&scoped_clone, &cmd_clone, None).await });

    wait_for_pending(&pm, 1).await?;
    let pending = pm.snapshot().pending;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tool, "file_write");

    let key = PermissionKey {
        tool: pending[0].tool.clone(),
        key: pending[0].key.clone(),
        value: pending[0].value.clone(),
    };
    pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
    let result = handle.await?;
    assert!(
        result.is_err(),
        "expected wildcard NOT to bypass file_write for mkdir, got {:?}",
        result
    );
    Ok(())
}

/// Test B — a standalone `cat <path>` (classified ReadCommand) must prompt
/// for `file_read` even when the bash wildcard is stored.
#[tokio::test]
async fn wildcard_does_not_bypass_file_read_for_read_command() -> Result<()> {
    let (pm, scoped) = pm_with_wildcard()?;
    // Real file required: check_file_read_permission errors out for
    // non-existent paths before any prompt is issued.
    let path = unique_temp_path("wild-cat-secret");
    tokio::fs::write(&path, "secret\n").await?;
    let cmd = format!("cat {}", path.display());

    let scoped_clone = scoped.clone();
    let cmd_clone = cmd.clone();
    let handle =
        tokio::spawn(async move { check_bash_permission(&scoped_clone, &cmd_clone, None).await });

    let result = wait_for_pending(&pm, 1).await;
    if result.is_err() {
        // Clean up before failing
        let _ = tokio::fs::remove_file(&path).await;
        result?;
    }
    let pending = pm.snapshot().pending;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tool, "file_read");

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
        "expected wildcard NOT to bypass file_read for cat, got {:?}",
        result
    );
    Ok(())
}

/// Test C — a top-level redirect (`echo hello > /tmp/foo`) must prompt for
/// `file_write` on the redirect target even when the bash wildcard is stored.
/// This validates that the redirect check runs *before* the classification
/// match (so the wildcard cannot save the OtherSimple `echo` from the
/// redirect-defense path).
#[tokio::test]
async fn wildcard_does_not_bypass_redirect_file_write() -> Result<()> {
    let (pm, scoped) = pm_with_wildcard()?;
    let path = unique_temp_path("wild-redirect-out");
    let cmd = format!("echo hello > {}", path.display());

    let scoped_clone = scoped.clone();
    let cmd_clone = cmd.clone();
    let handle =
        tokio::spawn(async move { check_bash_permission(&scoped_clone, &cmd_clone, None).await });

    wait_for_pending(&pm, 1).await?;
    let pending = pm.snapshot().pending;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].tool, "file_write");

    let key = PermissionKey {
        tool: pending[0].tool.clone(),
        key: pending[0].key.clone(),
        value: pending[0].value.clone(),
    };
    pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
    let result = handle.await?;
    assert!(
        result.is_err(),
        "expected wildcard NOT to bypass redirect file_write, got {:?}",
        result
    );
    Ok(())
}

/// Test D — a decomposable pipeline of OtherSimple commands (`ls | grep foo`)
/// is fully covered by the wildcard via the recursive Complex → decompose →
/// per-stage path. No prompts should fire.
#[tokio::test]
async fn wildcard_auto_approves_decomposable_pipeline_of_other_simple() -> Result<()> {
    let (pm, scoped) = pm_with_wildcard()?;
    let result = check_bash_permission(&scoped, "ls | grep foo", None).await;
    assert!(
        result.is_ok(),
        "expected pipeline to auto-approve via wildcard, got {:?}",
        result
    );
    assert!(
        pm.snapshot().pending.is_empty(),
        "expected no pending prompts, got {:?}",
        pm.snapshot().pending
    );
    Ok(())
}

/// Test E — a compound containing a write sub-command (`mkdir /tmp/foo && ls`)
/// must prompt for `file_write` on the mkdir target even with the wildcard.
/// After resolving the file_write with AllowSession, the recursion finishes
/// and the overall check returns Ok (because `ls` is wildcard-approved).
#[tokio::test]
async fn compound_with_write_prompts_for_file_write_only() -> Result<()> {
    let (pm, scoped) = pm_with_wildcard()?;
    let path = unique_temp_path("wild-compound-mkdir");
    let cmd = format!("mkdir {} && ls", path.display());

    let scoped_clone = scoped.clone();
    let cmd_clone = cmd.clone();
    let handle =
        tokio::spawn(async move { check_bash_permission(&scoped_clone, &cmd_clone, None).await });

    wait_for_pending(&pm, 1).await?;
    let pending = pm.snapshot().pending;
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].tool, "file_write",
        "first prompt should be file_write for mkdir target"
    );

    let key = PermissionKey {
        tool: pending[0].tool.clone(),
        key: pending[0].key.clone(),
        value: pending[0].value.clone(),
    };
    pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
    let result = handle.await?;
    assert!(
        result.is_ok(),
        "expected compound to succeed after granting file_write (ls is wildcard-approved), got {:?}",
        result
    );
    Ok(())
}

/// Test F — a *non-decomposable* complex command (command substitution) IS
/// auto-approved by the wildcard. This is the only blanket escape hatch
/// that remains, since the parser cannot see what `$(...)` will run.
#[tokio::test]
async fn non_decomposable_complex_auto_approved_with_wildcard() -> Result<()> {
    let (pm, scoped) = pm_with_wildcard()?;
    // `echo $(whoami)` parses as Complex (command_substitution descendant)
    // and is NOT decomposable (top node is `command`, not pipeline/list/
    // redirected_statement). See `command_parser_tests::
    // classify_command_substitution_as_complex` and the parser's
    // `try_decompose_complex` cases A/B/C/D.
    let result = check_bash_permission(&scoped, "echo $(whoami)", None).await;
    assert!(
        result.is_ok(),
        "expected non-decomposable complex to auto-approve via wildcard, got {:?}",
        result
    );
    assert!(
        pm.snapshot().pending.is_empty(),
        "expected no pending prompts, got {:?}",
        pm.snapshot().pending
    );
    Ok(())
}

/// Test G — same non-decomposable complex command WITHOUT the wildcard
/// produces a `once_only` pending request via `ask_permission_once`. This
/// replaces the old `complex_command_without_wildcard_still_prompts` test,
/// which used a *decomposable* command (`ls | grep foo > out.txt`) — that
/// command no longer reaches the once-only prompt path post-refactor.
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

    let scoped_clone = scoped.clone();
    let handle =
        tokio::spawn(
            async move { check_bash_permission(&scoped_clone, "echo $(whoami)", None).await },
        );

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

/// Test H — a compound containing a read sub-command
/// (`cat /tmp/secret && ls`) must prompt for `file_read` on the cat target
/// even with the wildcard.
#[tokio::test]
async fn compound_with_read_command_prompts_for_file_read() -> Result<()> {
    let (pm, scoped) = pm_with_wildcard()?;
    // The file must exist — `check_file_read_permission` errors out for
    // non-existent paths before any prompt is issued.
    let path = unique_temp_path("wild-compound-cat");
    tokio::fs::write(&path, "secret\n").await?;
    let cmd = format!("cat {} && ls", path.display());

    let scoped_clone = scoped.clone();
    let cmd_clone = cmd.clone();
    let handle =
        tokio::spawn(async move { check_bash_permission(&scoped_clone, &cmd_clone, None).await });

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
        "expected wildcard NOT to bypass file_read for cat in compound, got {:?}",
        result
    );
    Ok(())
}
