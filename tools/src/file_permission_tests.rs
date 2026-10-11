#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

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

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Result<Self> {
            let root = test_root();
            std::fs::create_dir_all(&root)?;
            let dir = root.join(uuid::Uuid::new_v4().to_string());
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            std::fs::create_dir_all(&dir)?;
            Ok(Self(dir))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = ?self.0, %error, "failed to clean up permission test directory");
            }
        }
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

    fn grant_session_root_read(pm: &PermissionManager, root: &Path) -> Result<PermissionKey> {
        let canonical_root = root.canonicalize()?;
        let key = make_key(
            "file_read",
            "path",
            tcode_encoding::path_to_str(&canonical_root)?,
        );
        pm.add_permission(key.clone(), PermissionScope::Session)?;
        Ok(key)
    }

    async fn assert_permission_denied(
        pm: &PermissionManager,
        check: impl Future<Output = Result<()>>,
        expected_key: &PermissionKey,
    ) -> Result<()> {
        tokio::pin!(check);
        tokio::select! {
            result = &mut check => {
                return Err(anyhow!("permission check completed without an approval request: {result:?}"));
            }
            result = wait_for_pending(pm, 1) => result?,
        }

        let state = pm.snapshot();
        assert_eq!(state.pending.len(), 1);
        let pending = &state.pending[0];
        assert_eq!(
            make_key(&pending.tool, &pending.key, &pending.value),
            *expected_key
        );
        pm.resolve(
            expected_key,
            &PermissionDecision::Deny { reason: None },
            None,
        )?;
        let result = tokio::time::timeout(Duration::from_secs(2), &mut check).await?;
        assert!(result.is_err(), "denied permission should return an error");
        assert!(pm.snapshot().pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn session_root_read_grant_covers_descendants_without_prompts() -> Result<()> {
        let fixture = TestDir::new()?;
        let root = fixture.path().join("sessions/session-a");
        std::fs::create_dir_all(&root)?;
        let pm = Arc::new(PermissionManager::new(
            fixture.path().join("permissions.json"),
        ));
        let root_key = grant_session_root_read(&pm, &root)?;
        let scoped = make_scoped(Arc::clone(&pm));

        // Create descendants after granting the root to exercise future session files.
        for relative in [
            "tool-logs/root.log",
            "subagent-child/tool-logs/child.log",
            "subagent-child/subagent-grandchild/tool-logs/nested.log",
            "conversation.json",
            "history.json",
            "media/image.png",
            "tool-events/event.json",
            "subagent-child/subagent-grandchild/history.json",
        ] {
            let file = root.join(relative);
            let parent = file.parent().expect("session descendants have a parent");
            std::fs::create_dir_all(parent)?;
            std::fs::write(&file, "session data")?;
            tokio::time::timeout(
                Duration::from_secs(2),
                check_file_read_permission(&scoped, &file, false),
            )
            .await??;
            tokio::time::timeout(
                Duration::from_secs(2),
                check_file_read_permission(&scoped, parent, true),
            )
            .await??;
        }

        let state = pm.snapshot();
        assert!(state.pending.is_empty());
        assert_eq!(state.session, vec![root_key]);
        assert!(state.project.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn session_root_read_grant_does_not_cover_parent_or_sibling_sessions() -> Result<()> {
        let fixture = TestDir::new()?;
        let sessions = fixture.path().join("sessions");
        let root = sessions.join("session-a");
        let sibling = sessions.join("session-a-sibling");
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&sibling)?;
        let sibling_file = sibling.join("history.json");
        std::fs::write(&sibling_file, "other session")?;
        let pm = Arc::new(PermissionManager::new(
            fixture.path().join("permissions.json"),
        ));
        let root_key = grant_session_root_read(&pm, &root)?;
        let scoped = make_scoped(Arc::clone(&pm));

        for (path, is_dir, permission_dir) in [
            (&sessions, true, &sessions),
            (&sibling, true, &sibling),
            (&sibling_file, false, &sibling),
        ] {
            let canonical_dir = permission_dir.canonicalize()?;
            let expected = make_key(
                "file_read",
                "path",
                tcode_encoding::path_to_str(&canonical_dir)?,
            );
            assert_permission_denied(
                &pm,
                check_file_read_permission(&scoped, path, is_dir),
                &expected,
            )
            .await?;
        }

        let state = pm.snapshot();
        assert_eq!(state.session, vec![root_key]);
        assert!(state.project.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn session_root_read_grant_does_not_authorize_writes() -> Result<()> {
        let fixture = TestDir::new()?;
        let root = fixture.path().join("sessions/session-a");
        std::fs::create_dir_all(&root)?;
        let existing = root.join("history.json");
        let new_file = root.join("new.json");
        std::fs::write(&existing, "session history")?;
        let pm = Arc::new(PermissionManager::new(
            fixture.path().join("permissions.json"),
        ));
        let root_key = grant_session_root_read(&pm, &root)?;
        let scoped = make_scoped(Arc::clone(&pm));

        let canonical_root = root.canonicalize()?;
        let cwd = std::env::current_dir()?.canonicalize()?;
        let permission_dir = if canonical_root.starts_with(&cwd) {
            &cwd
        } else {
            &canonical_root
        };
        let expected = make_key(
            "file_write",
            "path",
            tcode_encoding::path_to_str(permission_dir)?,
        );
        for path in [&existing, &new_file] {
            assert_permission_denied(
                &pm,
                check_file_write_permission(&scoped, path, "updated history", "json"),
                &expected,
            )
            .await?;
        }

        let state = pm.snapshot();
        assert_eq!(state.session, vec![root_key]);
        assert!(state.project.is_empty());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn session_root_read_grant_does_not_follow_symlinks_outside_root() -> Result<()> {
        let fixture = TestDir::new()?;
        let root = fixture.path().join("sessions/session-a");
        let outside = fixture.path().join("outside");
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&outside)?;
        let outside_file = outside.join("external.log");
        std::fs::write(&outside_file, "external data")?;
        let pm = Arc::new(PermissionManager::new(
            fixture.path().join("permissions.json"),
        ));
        let root_key = grant_session_root_read(&pm, &root)?;
        let scoped = make_scoped(Arc::clone(&pm));

        let linked_logs = root.join("tool-logs");
        let linked_file = root.join("external.log");
        std::os::unix::fs::symlink(&outside, &linked_logs)?;
        std::os::unix::fs::symlink(&outside_file, &linked_file)?;
        let log_file = linked_logs.join("external.log");
        let canonical_outside = outside.canonicalize()?;
        let expected = make_key(
            "file_read",
            "path",
            tcode_encoding::path_to_str(&canonical_outside)?,
        );
        for (path, is_dir) in [
            (&linked_logs, true),
            (&log_file, false),
            (&linked_file, false),
        ] {
            assert_permission_denied(
                &pm,
                check_file_read_permission(&scoped, path, is_dir),
                &expected,
            )
            .await?;
        }

        let state = pm.snapshot();
        assert_eq!(state.session, vec![root_key]);
        assert!(state.project.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn path_within_cwd_uses_session_permission() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));

        // Cwd read is no longer auto-allowed — add the expected session permission.
        let cwd = tokio::fs::canonicalize(std::env::current_dir()?).await?;
        let key = PermissionKey {
            tool: "file_read".to_string(),
            key: "path".to_string(),
            value: cwd.to_str().unwrap().to_string(),
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
            value: cwd_canonical.to_str().unwrap().to_string(),
        };
        pm.add_permission(key, PermissionScope::Session)?;

        let scoped = make_scoped(pm);

        let cwd = std::env::current_dir()?;
        let dir_name = cwd
            .file_name()
            .expect("cwd should have a file name")
            .to_str()
            .unwrap()
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
        let canonical_base_str = canonical_base.to_str().unwrap().to_string();

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
        let fixture = TestDir::new()?;
        let dir = fixture.path().join("session");
        std::fs::create_dir_all(&dir)?;
        let pm = Arc::new(PermissionManager::new(
            fixture.path().join("permissions.json"),
        ));
        let root_key = grant_session_root_read(&pm, &dir)?;

        for tool_name in ["read", "grep", "glob"] {
            let scoped = ScopedPermissionManager::new(
                tool_name,
                Arc::clone(&pm),
                Arc::new(|| {}),
                Arc::new(|| {}),
                None,
            );
            tokio::time::timeout(
                Duration::from_secs(2),
                check_file_read_permission(&scoped, &dir, true),
            )
            .await??;
        }

        let state = pm.snapshot();
        assert!(state.pending.is_empty());
        assert_eq!(state.session, vec![root_key]);
        assert!(state.project.is_empty());
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
        let key = make_key("file_read", "path", canonical.to_str().unwrap());
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
        let value = canonical.to_str().unwrap().to_string();
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
            &make_key("file_write", "path", canonical.to_str().unwrap()),
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

    #[tokio::test]
    async fn confined_manager_allows_inside_and_denies_outside_without_prompt() -> Result<()> {
        let fixture = TestDir::new()?;
        let root = fixture.path().join("session");
        let outside = fixture.path().join("outside");
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&outside)?;
        let inside_file = root.join("image.png");
        let outside_file = outside.join("secret.png");
        std::fs::write(&inside_file, "inside")?;
        std::fs::write(&outside_file, "outside")?;

        let pm = Arc::new(PermissionManager::new(
            fixture.path().join("permissions.json"),
        ));
        pm.set_file_root(Some(root.clone()))?;
        let root_key = grant_session_root_read(&pm, &root)?;
        let scoped = make_scoped(Arc::clone(&pm));

        // A path inside the file root is covered by the session-root grant.
        tokio::time::timeout(
            Duration::from_secs(2),
            check_file_read_permission(&scoped, &inside_file, false),
        )
        .await??;
        assert!(pm.snapshot().pending.is_empty());

        // A path outside the file root is denied with the confinement message
        // and never registers a prompt.
        let error = check_file_read_permission(&scoped, &outside_file, false)
            .await
            .expect_err("path outside the file root must be denied");
        assert!(
            error
                .to_string()
                .contains("web sessions can only access files inside the session directory"),
            "expected the confinement message, got: {error}"
        );
        assert!(pm.snapshot().pending.is_empty());
        assert_eq!(pm.snapshot().session, vec![root_key]);
        Ok(())
    }
}
