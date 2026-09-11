#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use anyhow::{Result, anyhow};
    use llm_rs::permission::{
        PermissionDecision, PermissionKey, PermissionManager, ScopedPermissionManager,
    };
    use llm_rs::tool::{CancellationToken, ToolContext};
    use tokio_stream::StreamExt;

    /// Name of the shared root under the workspace target dir. Every per-test
    /// directory lives below it; the root itself is never removed by a test.
    const MODULE: &str = "edit-tool";

    const NOT_FOUND: &str = "old_string was not found in the file. Make sure it matches exactly, \
                             including whitespace and indentation.";

    const NO_CHANGE: &str = "The replacement would not change the file. No changes made.";

    /// Per-test temp dir under the workspace target dir; removed on drop
    /// (cleanup runs on success and on panic).
    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let root =
                Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../target/test-tmp/{MODULE}"));
            std::fs::create_dir_all(&root).expect("failed to create test root");
            let dir = root.join(uuid::Uuid::new_v4().to_string());
            // Cleanup before: remove any stale leftover at this exact path.
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("failed to create test dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Build a `ToolContext` with `file_write` permission pre-granted for the
    /// canonical form of `dir`. The permission walk checks the file path and then
    /// its ancestors, so this single grant covers every file below `dir` and the
    /// tool never needs to prompt.
    fn make_ctx(dir: &Path, perm_path: &Path) -> Result<ToolContext> {
        let pm = Arc::new(PermissionManager::new(perm_path.to_path_buf()));
        let canonical_dir = dir.canonicalize()?;
        let value = canonical_dir
            .to_str()
            .ok_or_else(|| anyhow!("test dir is not valid UTF-8"))?
            .to_string();
        let key = PermissionKey {
            tool: "file_write".to_string(),
            key: "path".to_string(),
            value,
        };
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
        let scoped =
            ScopedPermissionManager::new("edit", pm, Arc::new(|| {}), Arc::new(|| {}), None);
        Ok(ToolContext {
            cancel_token: CancellationToken::new(),
            permission: scoped,
            container_config: None,
            session_dir: None,
            supports_media: false,
            llm: None,
            model: None,
        })
    }

    fn path_str(path: &Path) -> String {
        path.to_str()
            .expect("test-created path is valid UTF-8")
            .to_string()
    }

    /// Drain a tool's stream into a single result string, or the first error.
    async fn collect_stream(
        mut stream: impl tokio_stream::Stream<Item = Result<String>> + Unpin,
    ) -> Result<String> {
        let mut out = String::new();
        while let Some(item) = stream.next().await {
            out.push_str(&item?);
        }
        Ok(out)
    }

    /// Run the `edit` tool end to end against a real file on disk.
    async fn run_edit(
        ctx: ToolContext,
        file_path: &Path,
        old: &str,
        new: &str,
        replace_all: bool,
    ) -> Result<String> {
        let stream = crate::edit::edit(
            ctx,
            path_str(file_path),
            old.to_string(),
            new.to_string(),
            replace_all,
        );
        collect_stream(Box::pin(stream)).await
    }

    /// Number of line feeds that are not part of a Windows line ending.
    fn bare_lf_count(bytes: &[u8]) -> usize {
        let mut count = 0;
        for (i, byte) in bytes.iter().enumerate() {
            if *byte == b'\n' && (i == 0 || bytes[i - 1] != b'\r') {
                count += 1;
            }
        }
        count
    }

    /// A file must never contain a double carriage return: it is the signature of
    /// a conversion applied to text that already carried Windows endings.
    fn assert_no_double_cr(bytes: &[u8]) {
        assert!(
            !bytes.windows(3).any(|w| w == b"\r\r\n".as_slice()),
            "found a \\r\\r\\n sequence in {bytes:?}"
        );
    }

    /// Every line feed must be part of a Windows line ending.
    fn assert_all_newlines_are_crlf(bytes: &[u8]) {
        assert_eq!(
            bare_lf_count(bytes),
            0,
            "a line feed is not preceded by a carriage return in {bytes:?}"
        );
    }

    /// The exact success message the tool must produce for a given count.
    fn success_message(file_path: &Path, replacements: &str) -> String {
        format!(
            "Successfully edited {} ({})",
            path_str(file_path),
            replacements
        )
    }

    #[tokio::test]
    async fn crlf_file_edited_by_lf_search() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("crlf.txt");
        let original: &[u8] = b"line one\r\nline two\r\nline three\r\n";
        std::fs::write(&file_path, original)?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let message = run_edit(
            ctx,
            &file_path,
            "line two\nline three",
            "line two\nLINE THREE",
            false,
        )
        .await?;

        assert_eq!(message, success_message(&file_path, "1 replacement"));

        let after = std::fs::read(&file_path)?;
        assert_eq!(after, b"line one\r\nline two\r\nLINE THREE\r\n".as_slice());
        assert_all_newlines_are_crlf(&after);
        assert_no_double_cr(&after);

        // The bytes outside the replaced region are identical to the original.
        let prefix = b"line one\r\n";
        let suffix = b"\r\n";
        assert!(after.starts_with(prefix), "prefix changed: {after:?}");
        assert!(after.ends_with(suffix), "suffix changed: {after:?}");
        assert_eq!(&after[..prefix.len()], &original[..prefix.len()]);
        assert_eq!(
            &after[after.len() - suffix.len()..],
            &original[original.len() - suffix.len()..]
        );
        Ok(())
    }

    #[tokio::test]
    async fn inserted_lines_use_crlf() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("insert.txt");
        std::fs::write(&file_path, b"alpha\r\nbeta\r\ngamma\r\n")?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let message = run_edit(
            ctx,
            &file_path,
            "beta\ngamma",
            "beta\ninserted\ngamma",
            false,
        )
        .await?;

        assert_eq!(message, success_message(&file_path, "1 replacement"));

        let after = std::fs::read(&file_path)?;
        assert_eq!(after, b"alpha\r\nbeta\r\ninserted\r\ngamma\r\n".as_slice());
        assert_all_newlines_are_crlf(&after);
        assert_no_double_cr(&after);
        Ok(())
    }

    #[tokio::test]
    async fn lf_file_edited_as_before() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("unix.txt");
        std::fs::write(&file_path, b"line one\nline two\nline three\n")?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let message = run_edit(
            ctx,
            &file_path,
            "line two\nline three",
            "line two\nLINE THREE",
            false,
        )
        .await?;

        assert_eq!(message, success_message(&file_path, "1 replacement"));

        let after = std::fs::read(&file_path)?;
        assert_eq!(after, b"line one\nline two\nLINE THREE\n".as_slice());
        assert!(
            !after.contains(&b'\r'),
            "a Unix file must not gain carriage returns: {after:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn replace_all_reports_count() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("repeat.txt");
        std::fs::write(&file_path, b"foo\r\nbar\r\nfoo\r\nbar\r\nfoo\r\nbar\r\n")?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let message = run_edit(ctx, &file_path, "foo\nbar", "baz\nqux", true).await?;

        assert!(
            message.contains("3 replacement(s)"),
            "the count must come from the attempt that succeeded: {message}"
        );
        assert_eq!(message, success_message(&file_path, "3 replacement(s)"));

        let after = std::fs::read(&file_path)?;
        assert_eq!(
            after,
            b"baz\r\nqux\r\nbaz\r\nqux\r\nbaz\r\nqux\r\n".as_slice()
        );
        assert_all_newlines_are_crlf(&after);
        assert_no_double_cr(&after);
        Ok(())
    }

    #[tokio::test]
    async fn success_message_format_unchanged() -> Result<()> {
        let tdir = TestDir::new();

        let single = tdir.path().join("single.txt");
        std::fs::write(&single, b"one\r\ntwo\r\n")?;
        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm-single.json"))?;
        let message = run_edit(ctx, &single, "one\ntwo", "ONE\nTWO", false).await?;
        assert_eq!(
            message,
            format!("Successfully edited {} (1 replacement)", path_str(&single))
        );

        let all = tdir.path().join("all.txt");
        std::fs::write(&all, b"foo\r\nbar\r\nfoo\r\nbar\r\n")?;
        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm-all.json"))?;
        let message = run_edit(ctx, &all, "foo\nbar", "baz\nqux", true).await?;
        assert_eq!(
            message,
            format!("Successfully edited {} (2 replacement(s))", path_str(&all))
        );
        Ok(())
    }

    #[tokio::test]
    async fn no_op_leaves_file_untouched() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("noop.txt");
        let original: Vec<u8> = b"a\r\nb\r\n".to_vec();
        std::fs::write(&file_path, &original)?;
        let mtime_before = std::fs::metadata(&file_path)?.modified()?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let error = run_edit(ctx, &file_path, "a\nb", "a\r\nb", false)
            .await
            .expect_err("a replacement that changes no byte must be reported as a no-op");

        assert_eq!(error.to_string(), NO_CHANGE);
        assert_eq!(std::fs::read(&file_path)?, original);
        let mtime_after = std::fs::metadata(&file_path)?.modified()?;
        assert_eq!(mtime_before, mtime_after, "the file must not be written");
        Ok(())
    }

    #[tokio::test]
    async fn crlf_search_on_lf_file_not_found() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("unix.txt");
        let original: Vec<u8> = b"alpha\nbeta\n".to_vec();
        std::fs::write(&file_path, &original)?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let error = run_edit(ctx, &file_path, "alpha\r\nbeta", "A\r\nB", false)
            .await
            .expect_err("a Windows-endings search on a Unix file must not be found");

        assert_eq!(error.to_string(), NOT_FOUND);
        assert_eq!(std::fs::read(&file_path)?, original);
        Ok(())
    }

    #[tokio::test]
    async fn single_line_search_inserts_lf_into_crlf_file() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("single_line.txt");
        std::fs::write(&file_path, b"one\r\ntwo\r\nthree\r\n")?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let message = run_edit(ctx, &file_path, "two", "two\nand a half", false).await?;

        assert_eq!(message, success_message(&file_path, "1 replacement"));

        let after = std::fs::read(&file_path)?;
        assert_eq!(after, b"one\r\ntwo\nand a half\r\nthree\r\n".as_slice());
        // The inserted break is a Unix one: a single-line exact search keeps the
        // replacement exactly as supplied.
        assert!(
            after
                .windows(b"two\nand".len())
                .any(|w| w == b"two\nand".as_slice()),
            "the inserted line break must be a bare \\n: {after:?}"
        );
        // Every other line ending still belongs to the file's Windows style.
        assert_eq!(
            bare_lf_count(&after),
            1,
            "only the inserted line break may be a bare \\n: {after:?}"
        );
        assert_no_double_cr(&after);
        Ok(())
    }

    #[tokio::test]
    async fn ambiguous_search_is_not_retried() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("ambiguous.txt");
        let original: Vec<u8> = b"a\r\nb\r\na\r\nb\r\n".to_vec();
        std::fs::write(&file_path, &original)?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let error = run_edit(ctx, &file_path, "a\nb", "c\nd", false)
            .await
            .expect_err("a search matching twice after conversion must be ambiguous");

        assert_eq!(
            error.to_string(),
            "old_string appears 2 times in the file. Provide more surrounding context to make it \
             unique, or set replace_all to true."
        );
        assert_eq!(std::fs::read(&file_path)?, original);
        Ok(())
    }

    #[tokio::test]
    async fn not_found_error_wording() -> Result<()> {
        let tdir = TestDir::new();
        let file_path = tdir.path().join("absent.txt");
        let original: Vec<u8> = b"hello\r\nworld\r\n".to_vec();
        std::fs::write(&file_path, &original)?;

        let ctx = make_ctx(tdir.path(), &tdir.path().join("perm.json"))?;
        let error = run_edit(ctx, &file_path, "goodbye\nmoon", "greetings\nmoon", false)
            .await
            .expect_err("text present in neither style must be reported as not found");

        assert_eq!(error.to_string(), NOT_FOUND);
        assert_eq!(std::fs::read(&file_path)?, original);
        Ok(())
    }
}
