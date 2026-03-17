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
        let rx = pm.register_request("web_fetch", "Allow?", "hostname", "example.com");
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowOnce).unwrap();
        assert!(rx.await.unwrap());
        // AllowOnce does NOT persist to session
        assert!(!pm.has_permission("web_fetch", "hostname", "example.com"));
    }

    #[tokio::test]
    async fn register_and_resolve_allow_session() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let rx = pm.register_request("web_fetch", "Allow?", "hostname", "example.com");
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession).unwrap();
        assert!(rx.await.unwrap());
        // AllowSession persists in session
        assert!(pm.has_permission("web_fetch", "hostname", "example.com"));
    }

    #[tokio::test]
    async fn register_and_resolve_deny() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let rx = pm.register_request("web_fetch", "Allow?", "hostname", "evil.com");
        let key = make_key("web_fetch", "hostname", "evil.com");
        pm.resolve(&key, &PermissionDecision::Deny).unwrap();
        assert!(!rx.await.unwrap());
        // Deny does NOT persist
        assert!(!pm.has_permission("web_fetch", "hostname", "evil.com"));
    }

    #[tokio::test]
    async fn dedup_multiple_waiters() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let rx1 = pm.register_request("web_fetch", "Allow?", "hostname", "example.com");
        let rx2 = pm.register_request("web_fetch", "Allow?", "hostname", "example.com");
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession).unwrap();
        assert!(rx1.await.unwrap());
        assert!(rx2.await.unwrap());
    }

    #[tokio::test]
    async fn revoke_removes_permission() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let rx = pm.register_request("web_fetch", "Allow?", "hostname", "example.com");
        let key = make_key("web_fetch", "hostname", "example.com");
        pm.resolve(&key, &PermissionDecision::AllowSession).unwrap();
        assert!(rx.await.unwrap());
        assert!(pm.has_permission("web_fetch", "hostname", "example.com"));
        pm.revoke(&key).unwrap();
        assert!(!pm.has_permission("web_fetch", "hostname", "example.com"));
    }

    #[tokio::test]
    async fn close_all_pending_sends_false() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let rx1 = pm.register_request("web_fetch", "Allow?", "hostname", "a.com");
        let rx2 = pm.register_request("bash", "Allow?", "command", "git");
        pm.close_all_pending();
        assert!(!rx1.await.unwrap());
        assert!(!rx2.await.unwrap());
    }

    #[test]
    fn snapshot_includes_all_categories() {
        let pm = PermissionManager::new(temp_path());
        // Add a session permission directly
        let key = make_key("web_fetch", "hostname", "allowed.com");
        pm.resolve(&key, &PermissionDecision::AllowSession).unwrap();
        // Register a pending request
        let _rx = pm.register_request("bash", "Allow?", "command", "git");
        let state = pm.snapshot();
        assert_eq!(state.session.len(), 1);
        assert_eq!(state.pending.len(), 1);
        assert_eq!(state.pending[0].tool, "bash");
        assert_eq!(state.pending[0].key, "command");
        assert_eq!(state.pending[0].value, "git");
    }

    #[tokio::test]
    async fn project_persistence() {
        let path = temp_path();

        // Create PM, add project permission, drop it
        {
            let pm = PermissionManager::new(path.clone());
            let key = make_key("web_fetch", "hostname", "persisted.com");
            pm.resolve(&key, &PermissionDecision::AllowProject).unwrap();
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
        );
        assert!(!scoped.was_denied());

        // Deny the permission in the background
        let pm_clone = Arc::clone(&pm);
        let scoped_clone = scoped.clone();
        let handle = tokio::spawn(async move {
            let result = scoped_clone.ask_permission("Allow?", "hostname", "evil.com").await;
            assert!(!result);
        });
        // Give the task a moment to register the request
        tokio::task::yield_now().await;

        let key = make_key("web_fetch", "hostname", "evil.com");
        pm_clone.resolve(&key, &PermissionDecision::Deny).unwrap();
        handle.await.unwrap();

        assert!(scoped.was_denied());
    }

    #[tokio::test]
    async fn scoped_ask_permission_checks_stored_first() {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        // Pre-approve
        let key = make_key("web_fetch", "hostname", "known.com");
        pm.resolve(&key, &PermissionDecision::AllowSession).unwrap();

        let scoped = ScopedPermissionManager::new(
            "web_fetch",
            Arc::clone(&pm),
            Arc::new(|| {}),
            Arc::new(|| {}),
        );
        // Should return true immediately without registering a pending request
        assert!(scoped.ask_permission("Allow?", "hostname", "known.com").await);
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
        );

        // Spawn ask_permission in the background
        let pm_clone = Arc::clone(&pm);
        let scoped_clone = scoped.clone();
        let handle = tokio::spawn(async move {
            let result = scoped_clone.ask_permission("Allow?", "hostname", "example.com").await;
            assert!(result);
        });
        // Let the task register the request
        tokio::task::yield_now().await;

        let key = make_key("web_fetch", "hostname", "example.com");
        pm_clone.resolve(&key, &PermissionDecision::AllowOnce).unwrap();
        handle.await.unwrap();

        assert_eq!(approved_count.load(Ordering::Relaxed), 1);
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
        );

        let pm_clone = Arc::clone(&pm);
        let scoped_clone = scoped.clone();
        let handle = tokio::spawn(async move {
            let result = scoped_clone.ask_permission("Allow?", "hostname", "evil.com").await;
            assert!(!result);
        });
        tokio::task::yield_now().await;

        let key = make_key("web_fetch", "hostname", "evil.com");
        pm_clone.resolve(&key, &PermissionDecision::Deny).unwrap();
        handle.await.unwrap();

        assert_eq!(approved_count.load(Ordering::Relaxed), 0);
    }
}
