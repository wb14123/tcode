#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::Result;
    use llm_rs::permission::{
        PermissionDecision, PermissionKey, PermissionManager, ScopedPermissionManager,
    };
    use llm_rs::tool::{CancellationToken, ToolContext};
    use tokio_stream::StreamExt;

    fn test_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/write")
    }

    fn test_dir() -> std::path::PathBuf {
        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        dir
    }

    fn temp_perm_path() -> std::path::PathBuf {
        let dir = test_root().join(format!("perm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("failed to create temp perm dir");
        dir.join("permissions.json")
    }

    /// Build a ToolContext with file_write permission pre-granted for `dir`.
    ///
    /// Grants permission for the canonicalized `dir` itself. `has_ancestor_permission`
    /// walks up the directory tree, so any file created under `dir` is covered by
    /// this single grant. This works regardless of whether `dir` is inside cwd
    /// (where `widen_to_project_dir` would otherwise widen it).
    fn make_ctx_with_write_permission(dir: &std::path::Path) -> Result<ToolContext> {
        let pm = Arc::new(PermissionManager::new(temp_perm_path()));
        let canonical_dir = dir.canonicalize()?;
        let key = PermissionKey {
            tool: "file_write".to_string(),
            key: "path".to_string(),
            value: canonical_dir.to_string_lossy().to_string(),
        };
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
        let scoped =
            ScopedPermissionManager::new("write", pm, Arc::new(|| {}), Arc::new(|| {}), None);
        Ok(ToolContext {
            cancel_token: CancellationToken::new(),
            permission: scoped,
        })
    }

    /// Collect all stream items into a single result string or first error.
    async fn collect_stream(
        mut stream: impl tokio_stream::Stream<Item = Result<String>> + Unpin,
    ) -> Result<String> {
        let mut out = String::new();
        while let Some(item) = stream.next().await {
            out.push_str(&item?);
        }
        Ok(out)
    }

    #[tokio::test]
    async fn write_creates_new_file() -> Result<()> {
        let dir = test_dir();
        let file_path = dir.join("new_file.txt");

        let ctx = make_ctx_with_write_permission(&dir)?;
        let stream = crate::write::write(
            ctx,
            file_path.to_string_lossy().to_string(),
            "hello world\n".to_string(),
        );
        let result = collect_stream(Box::pin(stream)).await;
        assert!(result.is_ok(), "write should succeed: {:?}", result);
        let msg = result?;
        assert!(
            msg.contains("created new"),
            "message should say 'created new': {}",
            msg
        );

        let content = std::fs::read_to_string(&file_path)?;
        assert_eq!(content, "hello world\n");

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[tokio::test]
    async fn write_overwrites_existing_file() -> Result<()> {
        let dir = test_dir();
        let file_path = dir.join("existing.txt");
        std::fs::write(&file_path, "old content")?;

        let ctx = make_ctx_with_write_permission(&dir)?;
        let stream = crate::write::write(
            ctx,
            file_path.to_string_lossy().to_string(),
            "new content\n".to_string(),
        );
        let result = collect_stream(Box::pin(stream)).await;
        assert!(result.is_ok(), "write should succeed: {:?}", result);
        let msg = result?;
        assert!(
            msg.contains("overwrote existing"),
            "message should say 'overwrote existing': {}",
            msg
        );

        let content = std::fs::read_to_string(&file_path)?;
        assert_eq!(content, "new content\n");

        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    #[tokio::test]
    async fn rejects_relative_path() -> Result<()> {
        let ctx = ToolContext {
            cancel_token: CancellationToken::new(),
            permission: ScopedPermissionManager::always_allow("write"),
        };
        let stream =
            crate::write::write(ctx, "relative/path.txt".to_string(), "content".to_string());
        let result = collect_stream(Box::pin(stream)).await;
        assert!(result.is_err());
        let err = result
            .expect_err("expected error for relative path")
            .to_string();
        assert!(
            err.contains("absolute path"),
            "error should mention absolute path: {}",
            err
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_nonexistent_parent() -> Result<()> {
        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        // Do NOT create the directory
        let file_path = dir.join("no_parent").join("file.txt");

        let ctx = ToolContext {
            cancel_token: CancellationToken::new(),
            permission: ScopedPermissionManager::always_allow("write"),
        };
        let stream = crate::write::write(
            ctx,
            file_path.to_string_lossy().to_string(),
            "content".to_string(),
        );
        let result = collect_stream(Box::pin(stream)).await;
        assert!(result.is_err());
        let err = result
            .expect_err("expected error for nonexistent parent")
            .to_string();
        assert!(
            err.contains("Parent directory does not exist"),
            "error should mention parent: {}",
            err
        );
        Ok(())
    }
}
