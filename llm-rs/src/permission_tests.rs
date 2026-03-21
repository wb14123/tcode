#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::permission::{
        PermissionDecision, PermissionKey, PermissionManager, ScopedPermissionManager,
    };

    fn temp_path() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("perm-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
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
    async fn register_and_resolve_allow_once() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (request_id, rx) =
            pm.register_request("web_fetch", "Allow?", "hostname", "example.com", None);
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowOnce, Some(&request_id))
            .unwrap();
        assert!(rx.await.unwrap());
        // AllowOnce does NOT persist to session
        assert!(!pm.has_permission("web_fetch", "hostname", "example.com"));
    }

    #[tokio::test]
    async fn register_and_resolve_allow_session() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_request_id, rx) =
            pm.register_request("web_fetch", "Allow?", "hostname", "example.com", None);
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)
            .unwrap();
        assert!(rx.await.unwrap());
        // AllowSession persists in session
        assert!(pm.has_permission("web_fetch", "hostname", "example.com"));
    }

    #[tokio::test]
    async fn register_and_resolve_deny() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_request_id, rx) =
            pm.register_request("web_fetch", "Allow?", "hostname", "evil.com", None);
        let key = make_key("web_fetch", "hostname", "evil.com");
        pm.resolve(&key, &PermissionDecision::Deny, None).unwrap();
        assert!(!rx.await.unwrap());
        // Deny does NOT persist
        assert!(!pm.has_permission("web_fetch", "hostname", "evil.com"));
    }

    #[tokio::test]
    async fn dedup_multiple_waiters() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_rid1, rx1) =
            pm.register_request("web_fetch", "Allow?", "hostname", "example.com", None);
        let (_rid2, rx2) =
            pm.register_request("web_fetch", "Allow?", "hostname", "example.com", None);
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)
            .unwrap();
        assert!(rx1.await.unwrap());
        assert!(rx2.await.unwrap());
    }

    #[tokio::test]
    async fn allow_once_targets_single_waiter() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (rid1, rx1) =
            pm.register_request("web_fetch", "Allow?", "hostname", "example.com", None);
        let (_rid2, rx2) =
            pm.register_request("web_fetch", "Allow?", "hostname", "example.com", None);
        let key = make_key("web_fetch", "hostname", "example.com");
        // AllowOnce should only approve rid1, not rid2
        pm.resolve(&key, &PermissionDecision::AllowOnce, Some(&rid1))
            .unwrap();
        assert!(rx1.await.unwrap());
        // rx2 should still be pending (not resolved yet)
        // Resolve the remaining waiter with Deny
        pm.resolve(&key, &PermissionDecision::Deny, None).unwrap();
        assert!(!rx2.await.unwrap());
    }

    #[tokio::test]
    async fn allow_once_without_request_id_fails() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_rid, _rx) =
            pm.register_request("web_fetch", "Allow?", "hostname", "example.com", None);
        let key = make_key("web_fetch", "hostname", "example.com");
        let result = pm.resolve(&key, &PermissionDecision::AllowOnce, None);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn revoke_removes_permission() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_request_id, rx) =
            pm.register_request("web_fetch", "Allow?", "hostname", "example.com", None);
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)
            .unwrap();
        assert!(rx.await.unwrap());
        assert!(pm.has_permission("web_fetch", "hostname", "example.com"));
        pm.revoke(&key).unwrap();
        assert!(!pm.has_permission("web_fetch", "hostname", "example.com"));
    }

    #[tokio::test]
    async fn close_all_pending_sends_false() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let (_rid1, rx1) = pm.register_request("web_fetch", "Allow?", "hostname", "a.com", None);
        let (_rid2, rx2) = pm.register_request("bash", "Allow?", "command", "git", None);
        pm.close_all_pending();
        assert!(!rx1.await.unwrap());
        assert!(!rx2.await.unwrap());
    }

    #[test]
    fn snapshot_includes_all_categories() {
        let pm = PermissionManager::new(temp_path());
        // Add a session permission directly
        let key = make_key("web_fetch", "hostname", "allowed.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)
            .unwrap();
        // Register a pending request
        let (_rid, _rx) = pm.register_request("bash", "Allow?", "command", "git", None);
        let state = pm.snapshot();
        assert_eq!(state.session.len(), 1);
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].tool, "bash");
        assert_eq!(state.pending[0].key, "command");
        assert_eq!(state.pending[0].value, "git");
        assert!(!state.pending[0].request_id.is_empty());
    }

    #[tokio::test]
    async fn project_persistence() {
        let path = temp_path();

        // Create PM, add project permission, drop it
        {
            let pm = PermissionManager::new(path.clone());
            let key = make_key("web_fetch", "hostname", "persisted.com");
            pm.resolve(&key, &PermissionDecision::AllowProject, None)
                .unwrap();
            assert!(pm.has_permission("web_fetch", "hostname", "persisted.com"));
        }

        // Reload — project permission should survive
        {
            let pm = PermissionManager::new(path);
            assert!(pm.has_permission("web_fetch", "hostname", "persisted.com"));
        }
    }

    #[tokio::test]
    async fn scoped_was_denied_tracks_denial() {
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
        pm_clone
            .resolve(&key, &PermissionDecision::Deny, None)
            .unwrap();
        handle.await.unwrap();

        assert!(scoped.was_denied());
    }

    #[tokio::test]
    async fn scoped_ask_permission_checks_stored_first() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        // Pre-approve
        let key = make_key("web_fetch", "hostname", "known.com");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)
            .unwrap();

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
    }

    #[tokio::test]
    async fn on_approved_fn_called_when_permission_granted() {
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
        pm_clone
            .resolve(&key, &PermissionDecision::AllowOnce, Some(&request_id))
            .unwrap();
        handle.await.unwrap();

        assert_eq!(approved_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn has_permission_for_uses_custom_scope() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        // Grant permission under a custom scope
        let key = make_key("file_read", "path", "/outside/dir");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)
            .unwrap();

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
    }

    #[tokio::test]
    async fn ask_permission_for_uses_custom_scope() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        // Pre-approve under custom scope
        let key = make_key("file_read", "path", "/data");
        pm.resolve(&key, &PermissionDecision::AllowSession, None)
            .unwrap();

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
    }

    #[tokio::test]
    async fn ask_permission_for_registers_under_custom_scope() {
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
        pm_clone
            .resolve(&key, &PermissionDecision::AllowSession, None)
            .unwrap();
        assert!(handle.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn on_approved_fn_not_called_when_denied() {
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
        pm_clone
            .resolve(&key, &PermissionDecision::Deny, None)
            .unwrap();
        handle.await.unwrap();

        assert_eq!(approved_count.load(Ordering::Relaxed), 0);
    }
}
