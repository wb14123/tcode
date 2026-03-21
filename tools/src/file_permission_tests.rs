#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use anyhow::Result;
    use llm_rs::permission::{
        PermissionDecision, PermissionKey, PermissionManager, ScopedPermissionManager,
    };

    use crate::file_permission::check_file_read_permission;

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

    #[tokio::test]
    async fn path_within_cwd_needs_no_permission() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
        let scoped = make_scoped(pm);

        let cwd = std::env::current_dir()?;
        let result = check_file_read_permission(&scoped, &cwd, true).await;
        assert!(result.is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn path_traversal_neutralized_by_canonicalization() -> Result<()> {
        let pm = Arc::new(PermissionManager::new(temp_path()));
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
}
