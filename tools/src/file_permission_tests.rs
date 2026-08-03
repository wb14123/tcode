#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use anyhow::{Result, anyhow};
    use llm_rs::permission::{
        PermissionDecision, PermissionKey, PermissionManager, PermissionScope,
        ScopedPermissionManager,
    };

    use crate::file_permission::{check_file_read_permission, check_file_write_permission};

    fn test_root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/file_permission")
    }

    fn temp_path() -> std::path::PathBuf {
        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir.join("permissions.json")
    }

    fn make_scoped(pm: Arc<PermissionManager>) -> ScopedPermissionManager {
        ScopedPermissionManager::new("read", pm, Arc::new(|| {}), Arc::new(|| {}), None)
    }

    fn make_key(tool: &str, key: &str, value: &str) -> PermissionKey {
        PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    /// Poll `pm.snapshot().pending.len()` until it equals `expected`, returning Err on
    /// timeout. Used by tests that spawn a permission check in a task and need to
    /// wait for a prompt to register.
    async fn wait_for_pending(pm: &PermissionManager, expected: usize) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
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
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn path_within_cwd_uses_session_permission() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));

        // Cwd read is no longer auto-allowed — add the expected session permission.
        let cwd = tokio::fs::canonicalize(std::env::current_dir()?).await?;
        let key = PermissionKey {
            tool: "file_read".to_string(),
            key: "path".to_string(),
            value: cwd.to_string_lossy().to_string(),
        };
        pm.add_permission(key, PermissionScope::Session)?;

        let scoped = make_scoped(pm);

        let cwd_path = std::env::current_dir()?;
        let result = check_file_read_permission(&scoped, &cwd_path, true).await;
        assert!(result.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn path_traversal_neutralized_by_canonicalization() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));

        // Cwd read needs explicit permission now — add it.
        let cwd_canonical = tokio::fs::canonicalize(std::env::current_dir()?).await?;
        let key = PermissionKey {
            tool: "file_read".to_string(),
            key: "path".to_string(),
            value: cwd_canonical.to_string_lossy().to_string(),
        };
        pm.add_permission(key, PermissionScope::Session)?;

        let scoped = make_scoped(pm);

        let cwd = std::env::current_dir()?;
        let dir_name = cwd
            .file_name()
            .expect("cwd should have a file name")
            .to_string_lossy()
            .to_string();
        let traversal_path = cwd.join("..").join(&dir_name);
        let result = check_file_read_permission(&scoped, &traversal_path, true).await;
        assert!(result.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn hierarchical_parent_approval_covers_child() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));

        let base = test_root().join(uuid::Uuid::new_v4().to_string());
        let sub = base.join("subdir");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&sub)?;

        let canonical_base = tokio::fs::canonicalize(&base).await?;
        let canonical_base_str = canonical_base.to_string_lossy().to_string();

        let key = make_key("file_read", "path", &canonical_base_str);
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;

        let scoped = make_scoped(Arc::clone(&pm));

        let result = check_file_read_permission(&scoped, &sub, true).await;
        assert!(
            result.is_ok(),
            "child directory should be covered by parent approval"
        );
        assert!(pm.snapshot().pending.is_empty());

        let _ = std::fs::remove_dir_all(&base);
        Ok(())
    }

    #[tokio::test]
    async fn nonexistent_path_returns_error() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let scoped = make_scoped(pm);

        let nonexistent = test_root().join("definitely-does-not-exist");
        let _ = std::fs::remove_dir_all(&nonexistent);
        let result = check_file_read_permission(&scoped, &nonexistent, false).await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn unified_scope_works_across_tool_names() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));

        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let canonical_dir = tokio::fs::canonicalize(&dir).await?;
        let canonical_dir_str = canonical_dir.to_string_lossy().to_string();

        let key = make_key("file_read", "path", &canonical_dir_str);
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;

        let read_scoped = ScopedPermissionManager::new(
            "read",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );
        let result = check_file_read_permission(&read_scoped, &dir, true).await;
        assert!(result.is_ok(), "read tool should see file_read approval");

        let glob_scoped = ScopedPermissionManager::new(
            "glob",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );
        let result = check_file_read_permission(&glob_scoped, &dir, true).await;
        assert!(result.is_ok(), "glob tool should see file_read approval");

        assert!(pm.snapshot().pending.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[tokio::test]
    async fn exact_file_path_grant_matches_read() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));

        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        let file = dir.join("target.txt");
        std::fs::create_dir_all(&dir)?;
        tokio::fs::write(&file, "content").await?;

        // Grant file_read for the exact file path (not the parent directory).
        let canonical = tokio::fs::canonicalize(&file).await?;
        let key = make_key("file_read", "path", canonical.to_string_lossy().as_ref());
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;

        let scoped = make_scoped(Arc::clone(&pm));
        let result = check_file_read_permission(&scoped, &file, false).await;
        assert!(
            result.is_ok(),
            "exact-file grant should match, got {:?}",
            result
        );
        assert!(
            pm.snapshot().pending.is_empty(),
            "exact-file grant should not prompt"
        );

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[tokio::test]
    async fn dev_null_grants_pass_read_and_write() -> Result<()> {
        if !Path::new("/dev/null").exists() {
            // Non-Unix platform without /dev/null — nothing to test.
            return Ok(());
        }

        let pm = Arc::new(PermissionManager::new(temp_path()));
        let canonical = tokio::fs::canonicalize("/dev/null").await?;
        let value = canonical.to_string_lossy().to_string();
        for scope in ["file_read", "file_write"] {
            pm.resolve(
                &make_key(scope, "path", &value),
                &PermissionDecision::AllowSession,
                None,
            )?;
        }

        let scoped = make_scoped(Arc::clone(&pm));

        let read = check_file_read_permission(&scoped, Path::new("/dev/null"), false).await;
        assert!(
            read.is_ok(),
            "read /dev/null with grant should pass, got {:?}",
            read
        );

        let write = check_file_write_permission(&scoped, Path::new("/dev/null"), "", "bash").await;
        assert!(
            write.is_ok(),
            "write /dev/null with grant should pass, got {:?}",
            write
        );

        assert!(
            pm.snapshot().pending.is_empty(),
            "/dev/null grants should not prompt"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dev_null_grant_does_not_cover_sibling_devices() -> Result<()> {
        if !Path::new("/dev/null").exists() || !Path::new("/dev/zero").exists() {
            // Non-Unix platform without device files — nothing to test.
            return Ok(());
        }

        let pm = Arc::new(PermissionManager::new(temp_path()));
        let canonical = tokio::fs::canonicalize("/dev/null").await?;
        pm.resolve(
            &make_key("file_write", "path", canonical.to_string_lossy().as_ref()),
            &PermissionDecision::AllowSession,
            None,
        )?;

        let scoped = make_scoped(Arc::clone(&pm));
        let scoped_clone = scoped.clone();
        let handle = tokio::spawn(async move {
            check_file_write_permission(&scoped_clone, Path::new("/dev/zero"), "", "bash").await
        });

        // A /dev/null grant must NOT silently approve a write to a sibling
        // device file — /dev/zero should still prompt.
        wait_for_pending(&pm, 1).await?;
        let state = pm.snapshot();
        assert_eq!(state.pending.len(), 1);
        assert_eq!(
            state.pending[0].tool, "file_write",
            "write to /dev/zero should prompt despite the /dev/null grant"
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
            "expected file_write denial for /dev/zero to cause error, got {:?}",
            result
        );
        Ok(())
    }
}
