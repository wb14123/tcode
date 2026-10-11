#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::permission::{
        KEY_PATH, PermissionDecision, PermissionKey, PermissionManager, PermissionScope,
        ResolveOutcome, SCOPE_FILE_READ, SCOPE_FILE_WRITE, ScopedPermissionManager, WILDCARD_VALUE,
    };

    /// Per-test temp dir under the workspace target dir; removed on drop
    /// (cleanup runs on success and on panic).
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/test-tmp/permission");
            std::fs::create_dir_all(&root).expect("failed to create test root");
            let dir = root.join(uuid::Uuid::new_v4().to_string());
            // Cleanup before: remove any stale leftover at this exact path.
            if let Err(e) = std::fs::remove_dir_all(&dir)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!("Failed to clean stale test directory {dir:?}: {e}");
            }
            std::fs::create_dir_all(&dir).expect("failed to create test dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if let Err(e) = std::fs::remove_dir_all(&self.0) {
                tracing::warn!("Failed to remove test directory {:?}: {e}", self.0);
            }
        }
    }

    fn utf8(path: &std::path::Path) -> &str {
        path.to_str().expect("test path is valid UTF-8")
    }

    fn test_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/permission")
    }

    fn temp_path() -> std::path::PathBuf {
        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).expect("failed to create temp dir for test");
        dir.join("permissions.json")
    }

    fn make_key(tool: &str, key: &str, value: &str) -> PermissionKey {
        PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn has_permission_returns_false_initially() {
        let pm = PermissionManager::new(temp_path());
        assert!(!pm.has_permission("web_fetch", "hostname", "example.com"));
    }

    #[tokio::test]
    async fn register_and_resolve_allow_once() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (request_id, rx) = pm.register_request(
            "web_fetch",
            "Allow?",
            "hostname",
            "example.com",
            None,
            false,
        );
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowOnce, Some(&request_id))?;
        assert!(matches!(rx.await?, ResolveOutcome::Allowed));
        // AllowOnce does NOT persist to session
        assert!(!pm.has_permission("web_fetch", "hostname", "example.com"));
        Ok(())
    }

    #[tokio::test]
    async fn register_and_resolve_allow_session() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_request_id, rx) = pm.register_request(
            "web_fetch",
            "Allow?",
            "hostname",
            "example.com",
            None,
            false,
        );
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
        assert!(matches!(rx.await?, ResolveOutcome::Allowed));
        // AllowSession persists in session
        assert!(pm.has_permission("web_fetch", "hostname", "example.com"));
        Ok(())
    }

    #[tokio::test]
    async fn register_and_resolve_deny() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_request_id, rx) =
            pm.register_request("web_fetch", "Allow?", "hostname", "evil.com", None, false);
        let key = make_key("web_fetch", "hostname", "evil.com");
        pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
        assert!(matches!(rx.await?, ResolveOutcome::Denied(_)));
        // Deny does NOT persist
        assert!(!pm.has_permission("web_fetch", "hostname", "evil.com"));
        Ok(())
    }

    #[tokio::test]
    async fn dedup_multiple_waiters() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_rid1, rx1) = pm.register_request(
            "web_fetch",
            "Allow?",
            "hostname",
            "example.com",
            None,
            false,
        );
        let (_rid2, rx2) = pm.register_request(
            "web_fetch",
            "Allow?",
            "hostname",
            "example.com",
            None,
            false,
        );
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
        assert!(matches!(rx1.await?, ResolveOutcome::Allowed));
        assert!(matches!(rx2.await?, ResolveOutcome::Allowed));
        Ok(())
    }

    #[tokio::test]
    async fn allow_once_targets_single_waiter() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (rid1, rx1) = pm.register_request(
            "web_fetch",
            "Allow?",
            "hostname",
            "example.com",
            None,
            false,
        );
        let (_rid2, rx2) = pm.register_request(
            "web_fetch",
            "Allow?",
            "hostname",
            "example.com",
            None,
            false,
        );
        let key = make_key("web_fetch", "hostname", "example.com");
        // AllowOnce should only approve rid1, not rid2
        pm.resolve(&key, &PermissionDecision::AllowOnce, Some(&rid1))?;
        assert!(matches!(rx1.await?, ResolveOutcome::Allowed));
        // rx2 should still be pending (not resolved yet)
        // Resolve the remaining waiter with Deny
        pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
        assert!(matches!(rx2.await?, ResolveOutcome::Denied(_)));
        Ok(())
    }

    #[tokio::test]
    async fn allow_once_without_request_id_fails() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_rid, _rx) = pm.register_request(
            "web_fetch",
            "Allow?",
            "hostname",
            "example.com",
            None,
            false,
        );
        let key = make_key("web_fetch", "hostname", "example.com");
        let result = pm.resolve(&key, &PermissionDecision::AllowOnce, None);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn revoke_removes_permission() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_request_id, rx) = pm.register_request(
            "web_fetch",
            "Allow?",
            "hostname",
            "example.com",
            None,
            false,
        );
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
        assert!(matches!(rx.await?, ResolveOutcome::Allowed));
        assert!(pm.has_permission("web_fetch", "hostname", "example.com"));
        pm.revoke(&key)?;
        assert!(!pm.has_permission("web_fetch", "hostname", "example.com"));
        Ok(())
    }

    #[tokio::test]
    async fn close_all_pending_sends_cancelled() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_rid1, rx1) =
            pm.register_request("web_fetch", "Allow?", "hostname", "a.com", None, false);
        let (_rid2, rx2) = pm.register_request("bash", "Allow?", "command", "git", None, false);
        pm.close_all_pending();
        assert!(matches!(rx1.await?, ResolveOutcome::Cancelled));
        assert!(matches!(rx2.await?, ResolveOutcome::Cancelled));
        Ok(())
    }

    #[test]
    fn snapshot_includes_all_categories() -> anyhow::Result<()> {
        let pm = PermissionManager::new(temp_path());
        // Add a session permission directly
        let key = make_key("web_fetch", "hostname", "allowed.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
        // Register a pending request
        let (_rid, _rx) = pm.register_request("bash", "Allow?", "command", "git", None, false);
        let state = pm.snapshot();
        assert_eq!(state.session.len(), 1);
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].tool, "bash");
        assert_eq!(state.pending[0].key, "command");
        assert_eq!(state.pending[0].value, "git");
        assert!(!state.pending[0].request_id.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn project_persistence() -> anyhow::Result<()> {
        let path = temp_path();

        // Create PM, add project permission, drop it
        {
            let pm = PermissionManager::new(path.clone());
            let key = make_key("web_fetch", "hostname", "persisted.com");
            pm.resolve(&key, &PermissionDecision::AllowProject, None)?;
            assert!(pm.has_permission("web_fetch", "hostname", "persisted.com"));
        }

        // Reload — project permission should survive
        {
            let pm = PermissionManager::new(path);
            assert!(pm.has_permission("web_fetch", "hostname", "persisted.com"));
        }
        Ok(())
    }

    #[tokio::test]
    async fn scoped_was_denied_tracks_denial() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let scoped = ScopedPermissionManager::new(
            "web_fetch",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );
        assert!(!scoped.was_denied());

        // Deny the permission in the background
        let pm_clone = Arc::clone(&pm);
        let scoped_clone = scoped.clone();
        let handle = tokio::spawn(async move {
            let result = scoped_clone
                .ask_permission("Allow?", "hostname", "evil.com")
                .await;
            assert!(result.is_err());
        });
        // Give the task a moment to register the request
        tokio::task::yield_now().await;

        let key = make_key("web_fetch", "hostname", "evil.com");
        pm_clone.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
        handle.await?;

        assert!(scoped.was_denied());
        Ok(())
    }

    #[tokio::test]
    async fn scoped_ask_permission_checks_stored_first() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        // Pre-approve
        let key = make_key("web_fetch", "hostname", "known.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;

        let scoped = ScopedPermissionManager::new(
            "web_fetch",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );
        // Should return Ok immediately without registering a pending request
        assert!(
            scoped
                .ask_permission("Allow?", "hostname", "known.com")
                .await
                .is_ok()
        );
        assert!(pm.snapshot().pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn on_approved_fn_called_when_permission_granted() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pm = Arc::new(PermissionManager::new(temp_path()));
        let approved_count = Arc::new(AtomicUsize::new(0));
        let approved_count_clone = Arc::clone(&approved_count);

        let scoped = ScopedPermissionManager::new(
            "web_fetch",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(move || {
                approved_count_clone.fetch_add(1, Ordering::Relaxed);
            }),
            None,
        );

        // Spawn ask_permission in the background
        let pm_clone = Arc::clone(&pm);
        let scoped_clone = scoped.clone();
        let handle = tokio::spawn(async move {
            let result = scoped_clone
                .ask_permission("Allow?", "hostname", "example.com")
                .await;
            assert!(result.is_ok());
        });
        // Let the task register the request
        tokio::task::yield_now().await;

        let key = make_key("web_fetch", "hostname", "example.com");
        // Use the request_id from the snapshot to do AllowOnce
        let request_id = pm_clone.snapshot().pending[0].request_id.clone();
        pm_clone.resolve(&key, &PermissionDecision::AllowOnce, Some(&request_id))?;
        handle.await?;

        assert_eq!(approved_count.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[test]
    fn has_permission_for_uses_custom_scope() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        // Grant permission under a custom scope
        let key = make_key("file_read", "path", "/outside/dir");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;

        let scoped = ScopedPermissionManager::new(
            "read",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );

        // has_permission (tool_name = "read") should NOT find it
        assert!(!scoped.has_permission("path", "/outside/dir"));
        // has_permission_for with correct scope should find it
        assert!(scoped.has_permission_for("file_read", "path", "/outside/dir"));
        Ok(())
    }

    #[tokio::test]
    async fn ask_permission_for_uses_custom_scope() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        // Pre-approve under custom scope
        let key = make_key("file_read", "path", "/data");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;

        let scoped = ScopedPermissionManager::new(
            "glob",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );

        // ask_permission_for with correct scope returns immediately
        assert!(
            scoped
                .ask_permission_for("file_read", "Allow?", "path", "/data")
                .await
                .is_ok()
        );
        assert!(pm.snapshot().pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn ask_permission_for_registers_under_custom_scope() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let scoped = ScopedPermissionManager::new(
            "read",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );

        let pm_clone = Arc::clone(&pm);
        let scoped_clone = scoped.clone();
        let handle = tokio::spawn(async move {
            scoped_clone
                .ask_permission_for("file_read", "Allow?", "path", "/secret")
                .await
        });
        tokio::task::yield_now().await;

        // Pending request should be under "file_read", not "read"
        let state = pm_clone.snapshot();
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].tool, "file_read");

        // Resolve and clean up
        let key = make_key("file_read", "path", "/secret");
        pm_clone.resolve(&key, &PermissionDecision::AllowSession, None)?;
        assert!(handle.await?.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn on_approved_fn_not_called_when_denied() -> anyhow::Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let pm = Arc::new(PermissionManager::new(temp_path()));
        let approved_count = Arc::new(AtomicUsize::new(0));
        let approved_count_clone = Arc::clone(&approved_count);

        let scoped = ScopedPermissionManager::new(
            "web_fetch",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(move || {
                approved_count_clone.fetch_add(1, Ordering::Relaxed);
            }),
            None,
        );

        let pm_clone = Arc::clone(&pm);
        let scoped_clone = scoped.clone();
        let handle = tokio::spawn(async move {
            let result = scoped_clone
                .ask_permission("Allow?", "hostname", "evil.com")
                .await;
            assert!(result.is_err());
        });
        tokio::task::yield_now().await;

        let key = make_key("web_fetch", "hostname", "evil.com");
        pm_clone.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;
        handle.await?;

        assert_eq!(approved_count.load(Ordering::Relaxed), 0);
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Wildcard permission tests (see plan.md §Test plan, tests 1–6 and 9).
    // ---------------------------------------------------------------------

    #[test]
    fn wildcard_preserves_exact_match() -> anyhow::Result<()> {
        let pm = PermissionManager::new(temp_path());
        pm.add_permission(make_key("bash", "command", "git"), PermissionScope::Session)?;
        pm.add_permission(
            make_key("bash", "command", WILDCARD_VALUE),
            PermissionScope::Session,
        )?;
        assert!(pm.has_permission("bash", "command", "git"));
        Ok(())
    }

    #[test]
    fn wildcard_matches_arbitrary_value() -> anyhow::Result<()> {
        let pm = PermissionManager::new(temp_path());
        pm.add_permission(
            make_key("bash", "command", WILDCARD_VALUE),
            PermissionScope::Session,
        )?;
        assert!(pm.has_permission("bash", "command", "anything-else"));
        Ok(())
    }

    #[test]
    fn no_wildcard_no_exact_match_returns_false() -> anyhow::Result<()> {
        let pm = PermissionManager::new(temp_path());
        pm.add_permission(make_key("bash", "command", "git"), PermissionScope::Session)?;
        assert!(!pm.has_permission("bash", "command", "ls"));
        Ok(())
    }

    #[test]
    fn wildcard_scoped_to_tool_and_key() -> anyhow::Result<()> {
        let pm = PermissionManager::new(temp_path());
        pm.add_permission(
            make_key("bash", "command", WILDCARD_VALUE),
            PermissionScope::Session,
        )?;
        // Different tool — wildcard must not apply.
        assert!(!pm.has_permission("file_read", "path", "/tmp"));
        // Same tool but different key — wildcard must not apply.
        assert!(!pm.has_permission("bash", "hostname", "x"));
        Ok(())
    }

    #[test]
    fn wildcard_works_across_session_and_project() -> anyhow::Result<()> {
        // Session scope
        {
            let pm = PermissionManager::new(temp_path());
            pm.add_permission(
                make_key("bash", "command", WILDCARD_VALUE),
                PermissionScope::Session,
            )?;
            assert!(pm.has_permission("bash", "command", "anything"));
        }
        // Fresh manager, project scope
        {
            let pm = PermissionManager::new(temp_path());
            pm.add_permission(
                make_key("bash", "command", WILDCARD_VALUE),
                PermissionScope::Project,
            )?;
            assert!(pm.has_permission("bash", "command", "anything"));
        }
        Ok(())
    }

    #[test]
    fn revoking_wildcard_leaves_specific_entries_intact() -> anyhow::Result<()> {
        let pm = PermissionManager::new(temp_path());
        pm.add_permission(make_key("bash", "command", "git"), PermissionScope::Session)?;
        pm.add_permission(
            make_key("bash", "command", WILDCARD_VALUE),
            PermissionScope::Session,
        )?;
        // Sanity check: wildcard lets "ls" through before revocation.
        assert!(pm.has_permission("bash", "command", "ls"));

        pm.revoke(&make_key("bash", "command", WILDCARD_VALUE))?;

        // Specific entry survives.
        assert!(pm.has_permission("bash", "command", "git"));
        // Wildcard is gone — "ls" no longer matches.
        assert!(!pm.has_permission("bash", "command", "ls"));
        Ok(())
    }

    #[tokio::test]
    async fn ask_permission_with_wildcard_value_is_hard_error() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let scoped = ScopedPermissionManager::new(
            "bash",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );

        let result = scoped
            .ask_permission_for("bash", "prompt", "command", WILDCARD_VALUE)
            .await;
        assert!(
            result.is_err(),
            "ask_permission_for with wildcard must hard-error"
        );

        let result = scoped
            .ask_permission_with_preview(
                "bash",
                "prompt",
                "command",
                WILDCARD_VALUE,
                "content",
                "sh",
            )
            .await;
        assert!(
            result.is_err(),
            "ask_permission_with_preview with wildcard must hard-error"
        );

        // ask_permission_once routes through the same ask_permission_inner guard.
        // Its signature is (scope, prompt, content, file_extension) — it hard-codes
        // key="command" and uses `content` as the value, so we pass WILDCARD_VALUE
        // as `content` to exercise the guard.
        let result = scoped
            .ask_permission_once("bash", "Allow?", WILDCARD_VALUE, "sh")
            .await;
        assert!(
            result.is_err(),
            "ask_permission_once with value '*' should error"
        );

        // No pending request should have been registered.
        assert!(pm.snapshot().pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn wildcard_roundtrips_through_project_file() -> anyhow::Result<()> {
        let path = temp_path();

        // Create PM, add wildcard at Project scope (triggers disk write), drop it.
        {
            let pm = PermissionManager::new(path.clone());
            pm.add_permission(
                make_key("bash", "command", WILDCARD_VALUE),
                PermissionScope::Project,
            )?;
            assert!(pm.has_permission("bash", "command", "anything"));
        }

        // Reload — wildcard must survive the round-trip through JSON.
        {
            let pm = PermissionManager::new(path.clone());
            assert!(
                pm.has_permission("bash", "command", "anything"),
                "wildcard should still match after reload from disk"
            );

            // Revoke the wildcard (another disk write) and drop.
            pm.revoke(&make_key("bash", "command", WILDCARD_VALUE))?;
        }

        // Reload again — wildcard should be gone.
        {
            let pm = PermissionManager::new(path);
            assert!(
                !pm.has_permission("bash", "command", "anything"),
                "revoked wildcard should not re-appear after reload"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn deny_with_reason_surfaces_in_error_text() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let scoped = ScopedPermissionManager::new(
            "bash",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );

        // Spawn the permission wait on a background task.
        let scoped_clone = scoped.clone();
        let waiter = tokio::spawn(async move {
            scoped_clone
                .ask_permission("Allow running ls?", "command", "ls")
                .await
        });

        // Give the task a moment to register the pending request.
        tokio::task::yield_now().await;

        // Resolve with a deny reason.
        let key = make_key("bash", "command", "ls");
        pm.resolve(
            &key,
            &PermissionDecision::Deny {
                reason: Some("because".to_string()),
            },
            None,
        )?;

        // The waiter should have returned an Err whose Display matches the
        // reason-path error text byte-for-byte. The scoped manager still
        // tracks `was_denied` so the caller can classify the tool result
        // as UserDenied.
        let res = waiter.await?;
        let err = res.expect_err("expected Err from denied permission");
        let expected = "Permission denied: Allow running ls? \
                        The user chose not to allow this action. \
                        The user's reason: because";
        assert_eq!(format!("{err}"), expected);
        assert!(scoped.was_denied());
        Ok(())
    }

    #[tokio::test]
    async fn deny_without_reason_error_text_is_byte_exact() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let scoped = ScopedPermissionManager::new(
            "bash",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );

        let scoped_clone = scoped.clone();
        let waiter = tokio::spawn(async move {
            scoped_clone
                .ask_permission("Allow running ls?", "command", "ls")
                .await
        });
        tokio::task::yield_now().await;

        let key = make_key("bash", "command", "ls");
        pm.resolve(&key, &PermissionDecision::Deny { reason: None }, None)?;

        let res = waiter.await?;
        let err = res.expect_err("expected Err from denied permission");
        // Byte-for-byte invariant: the no-reason error text must exactly
        // equal the pre-deny-reason wording, with no trailing reason suffix.
        let expected =
            "Permission denied: Allow running ls? The user chose not to allow this action.";
        assert_eq!(format!("{err}"), expected);
        Ok(())
    }

    #[tokio::test]
    async fn deny_reason_whitespace_is_sanitized() -> anyhow::Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let scoped = ScopedPermissionManager::new(
            "bash",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );

        let scoped_clone = scoped.clone();
        let waiter = tokio::spawn(async move {
            scoped_clone
                .ask_permission("Allow running ls?", "command", "ls")
                .await
        });
        tokio::task::yield_now().await;

        let key = make_key("bash", "command", "ls");
        pm.resolve(
            &key,
            &PermissionDecision::Deny {
                reason: Some("\nuse\tglob\n\ninstead\n".to_string()),
            },
            None,
        )?;

        let res = waiter.await?;
        let err = res.expect_err("expected Err from denied permission");
        // Newlines/tabs/multi-space runs all collapse to a single space; the
        // single-line layout invariant must hold downstream. Byte-exact so a
        // stray double-space (e.g. from `"use  glob instead"`) would fail.
        let expected = "Permission denied: Allow running ls? \
                        The user chose not to allow this action. \
                        The user's reason: use glob instead";
        assert_eq!(format!("{err}"), expected);
        Ok(())
    }

    // ---------------------------------------------------------------------
    // File-root confinement (web-only sessions).
    // ---------------------------------------------------------------------

    /// Build a `file_root` directory plus a sibling path guaranteed to be
    /// outside it, both canonicalized.
    fn root_and_outside(dir: &TestDir) -> anyhow::Result<(PathBuf, PathBuf)> {
        let root = std::fs::canonicalize(dir.path())?;
        let outside = root
            .parent()
            .expect("canonical test dir always has a parent")
            .to_path_buf();
        Ok((root, outside))
    }

    #[test]
    fn file_root_denies_outside_but_honours_inside_grants() -> anyhow::Result<()> {
        let dir = TestDir::new();
        let (root, outside) = root_and_outside(&dir)?;
        let inside = root.join("media");
        std::fs::create_dir_all(&inside)?;
        let inside = std::fs::canonicalize(&inside)?;

        let pm = PermissionManager::new(root.join("permissions.json"));
        pm.set_file_root(Some(root.clone()))?;

        // A stored grant for an inside path is still honoured.
        pm.add_permission(
            make_key(SCOPE_FILE_READ, KEY_PATH, utf8(&inside)),
            PermissionScope::Session,
        )?;
        assert!(pm.has_permission(SCOPE_FILE_READ, KEY_PATH, utf8(&inside)));

        // An outside path is denied even when a matching grant is stored.
        pm.add_permission(
            make_key(SCOPE_FILE_READ, KEY_PATH, utf8(&outside)),
            PermissionScope::Session,
        )?;
        assert!(!pm.has_permission(SCOPE_FILE_READ, KEY_PATH, utf8(&outside)));

        Ok(())
    }

    #[test]
    fn file_root_confines_only_file_path_checks() -> anyhow::Result<()> {
        let dir = TestDir::new();
        let (root, outside) = root_and_outside(&dir)?;
        let outside = utf8(&outside).to_string();

        let pm = PermissionManager::new(root.join("permissions.json"));
        pm.set_file_root(Some(root.clone()))?;

        // A non-file scope is unaffected.
        pm.add_permission(
            make_key("web_fetch", "hostname", &outside),
            PermissionScope::Session,
        )?;
        assert!(pm.has_permission("web_fetch", "hostname", &outside));

        // A file scope under a different key is unaffected.
        pm.add_permission(
            make_key(SCOPE_FILE_READ, "other_key", &outside),
            PermissionScope::Session,
        )?;
        assert!(pm.has_permission(SCOPE_FILE_READ, "other_key", &outside));

        // A wildcard file-read grant does NOT open an outside concrete path:
        // confinement is applied before the stored-grant lookup.
        pm.add_permission(
            make_key(SCOPE_FILE_READ, KEY_PATH, WILDCARD_VALUE),
            PermissionScope::Session,
        )?;
        assert!(!pm.has_permission(SCOPE_FILE_READ, KEY_PATH, &outside));

        // The wildcard value itself is skipped by the confinement check (a
        // caller may query the wildcard entry directly).
        assert!(pm.has_permission(SCOPE_FILE_READ, KEY_PATH, WILDCARD_VALUE));

        Ok(())
    }

    #[test]
    fn set_file_root_none_restores_unrestricted_access() -> anyhow::Result<()> {
        let dir = TestDir::new();
        let (root, outside) = root_and_outside(&dir)?;
        let outside = utf8(&outside).to_string();

        let pm = PermissionManager::new(root.join("permissions.json"));
        pm.add_permission(
            make_key(SCOPE_FILE_READ, KEY_PATH, &outside),
            PermissionScope::Session,
        )?;

        pm.set_file_root(Some(root))?;
        assert!(!pm.has_permission(SCOPE_FILE_READ, KEY_PATH, &outside));

        pm.set_file_root(None)?;
        assert!(pm.has_permission(SCOPE_FILE_READ, KEY_PATH, &outside));

        Ok(())
    }

    #[tokio::test]
    async fn ask_permission_for_outside_root_errors_without_pending() -> anyhow::Result<()> {
        let dir = TestDir::new();
        let (root, outside) = root_and_outside(&dir)?;

        let pm = Arc::new(PermissionManager::new(root.join("permissions.json")));
        pm.set_file_root(Some(root))?;
        let scoped = ScopedPermissionManager::new(
            "read",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            None,
        );

        let err = scoped
            .ask_permission_for(SCOPE_FILE_READ, "Allow?", KEY_PATH, utf8(&outside))
            .await
            .expect_err("a path outside the file root must be denied");
        assert!(
            err.to_string().contains("inside the session directory"),
            "unexpected confinement error: {err}"
        );
        assert!(
            pm.snapshot().pending.is_empty(),
            "no pending request may be recorded for a confined path"
        );

        Ok(())
    }

    #[tokio::test]
    async fn ask_permission_with_preview_outside_root_cleans_preview() -> anyhow::Result<()> {
        let dir = TestDir::new();
        let (root, outside) = root_and_outside(&dir)?;
        let outside = utf8(&outside).to_string();

        let pm = Arc::new(PermissionManager::new(root.join("permissions.json")));
        pm.set_file_root(Some(root.clone()))?;
        let scoped = ScopedPermissionManager::new(
            "write",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
            Some(root.clone()),
        );

        let err = scoped
            .ask_permission_with_preview(
                SCOPE_FILE_WRITE,
                "Allow?",
                KEY_PATH,
                &outside,
                "content",
                "txt",
            )
            .await
            .expect_err("a path outside the file root must be denied");
        assert!(err.to_string().contains("inside the session directory"));
        assert!(pm.snapshot().pending.is_empty());

        // The preview file must have been cleaned up on the confinement denial.
        let preview_dir = root.join("tool-file-preview");
        if preview_dir.exists() {
            assert_eq!(
                std::fs::read_dir(&preview_dir)?.count(),
                0,
                "preview file must be removed when confinement denies the path"
            );
        }

        Ok(())
    }
}
