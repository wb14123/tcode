#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;

    use anyhow::Result;
    use llm_rs::media::ContentPart;
    use llm_rs::permission::{
        PermissionDecision, PermissionKey, PermissionManager, ScopedPermissionManager,
    };
    use llm_rs::tool::{CancellationToken, ToolContext};
    use tokio_stream::StreamExt;

    fn test_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/read")
    }

    fn temp_dir() -> std::path::PathBuf {
        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        dir
    }

    fn temp_perm_path() -> std::path::PathBuf {
        let dir = test_root().join(format!("perm-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("failed to create temp perm dir");
        dir.join("permissions.json")
    }

    /// Per-test temp dir under the workspace target dir; removed on drop
    /// (cleanup runs on success and on panic).
    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(module: &str) -> Self {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../target/test-tmp/{module}"));
            std::fs::create_dir_all(&root).expect("failed to create test root");
            let dir = root.join(uuid::Uuid::new_v4().to_string());
            // Cleanup before: remove any stale leftover at this exact path.
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("failed to create test dir");
            Self(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Build a ToolContext with file_read permission pre-granted for `dir`.
    fn make_ctx_with_read_permission(dir: &std::path::Path) -> Result<ToolContext> {
        make_ctx_with_read_permission_at(dir, &temp_perm_path())
    }

    /// Like `make_ctx_with_read_permission`, but with a caller-provided permissions
    /// file (e.g. inside a `TestDir` so the RAII guard cleans it up too).
    fn make_ctx_with_read_permission_at(
        dir: &std::path::Path,
        perm_path: &std::path::Path,
    ) -> Result<ToolContext> {
        let pm = Arc::new(PermissionManager::new(perm_path.to_path_buf()));
        let canonical_dir = dir.canonicalize()?;
        let key = PermissionKey {
            tool: "file_read".to_string(),
            key: "path".to_string(),
            value: canonical_dir.to_str().unwrap().to_string(),
        };
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
        let scoped =
            ScopedPermissionManager::new("read", pm, Arc::new(|| {}), Arc::new(|| {}), None);
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

    /// Collect all stream items into a single string (or first error).
    async fn collect_stream(
        stream: impl tokio_stream::Stream<Item = Result<ContentPart>>,
    ) -> Result<String> {
        tokio::pin!(stream);
        let mut out = String::new();
        while let Some(item) = stream.next().await {
            match item? {
                ContentPart::Text(text) => out.push_str(&text),
                ContentPart::Media(media) => {
                    out.push_str(&format!(
                        "[Image: {} {}]",
                        media.relative_path(),
                        media.media_type()
                    ));
                }
            }
        }
        Ok(out)
    }

    // ── Test 1: Normal file read ──────────────────────────────────────────

    #[tokio::test]
    async fn normal_file_read() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("normal.txt");
        std::fs::write(&file_path, "line one\nline two\nline three\n")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        let expected_header = format!("#| File: {}", file_path.to_str().unwrap());
        assert!(
            output.starts_with(&expected_header),
            "output should start with header.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("\n1| line one\n"),
            "line 1 should have prefix.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("\n2| line two\n"),
            "line 2 should have prefix.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("\n3| line three\n"),
            "line 3 should have prefix.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Lines 1-3 of 3 total."),
            "should have correct footer.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 2: Truncated line (buffer cap) ───────────────────────────────

    #[tokio::test]
    async fn truncated_line_buffer_cap() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("long_line.txt");

        // Create a file with a single very long line (no newline), 2000 'A's
        let mut file = std::fs::File::create(&file_path)?;
        for _ in 0..2000 {
            file.write_all(b"A")?;
        }

        let ctx = make_ctx_with_read_permission(&dir)?;
        // Set max_read_chars to 100 so the buffer cap kicks in
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            Some(100),
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_str().unwrap())),
            "should have header.\nGot:\n{}",
            output
        );
        // Verify the content line contains exactly 100 'A's after the "1| " prefix
        assert!(
            output.contains("1| AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            "content line should have truncated content.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Line 1 above is truncated at character 100."),
            "should have truncation annotation.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| To continue, re-read with first_line_offset=100."),
            "should have re-read advice.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 3: first_line_offset ─────────────────────────────────────────

    #[tokio::test]
    async fn first_line_offset() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("offset.txt");

        // Single line: "ABCDEFGHIJ" (10 chars)
        std::fs::write(&file_path, "ABCDEFGHIJ")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            None,
            Some(5), // skip first 5 chars
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("1| FGHIJ"),
            "line should start at character offset 5.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Line 1 above starts at character 5."),
            "should have offset annotation.\nGot:\n{}",
            output
        );
        // No "truncated" should appear
        assert!(
            !output.contains("truncated"),
            "should not have truncation annotation.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 4: Both offset + truncation ──────────────────────────────────
    // Use a newline-terminated line so the buffer cap does NOT kick in
    // (the buffer cap would truncate before first_line_offset is applied).

    #[tokio::test]
    async fn offset_and_truncation() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("offset_trunc.txt");

        // Single newline-terminated line: 26 chars + newline
        std::fs::write(&file_path, "ABCDEFGHIJKLMNOPQRSTUVWXYZ\n")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        // offset=5 skips "ABCDE", max_read_chars=15 limits output
        // After offset: "FGHIJKLMNOPQRSTUVWXYZ" (21 chars), truncated at 15: "FGHIJKLMNOPQRSTU"
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            Some(15),
            Some(5),
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("1| FGHIJKLMNOPQRST"),
            "line should be offset and truncated.\nGot:\n{}",
            output
        );
        assert!(
            output.contains(
                "#| Line 1 above starts at character 5 and is truncated at character 20."
            ),
            "should have combined offset+truncation annotation.\nGot:\n{}",
            output
        );
        // Per-line "To continue" is not emitted for global char cap truncation.
        // The footer-level "To read more" handles re-read advice instead.
        assert!(
            !output.contains("#| To continue,"),
            "should NOT have per-line 'To continue' for global cap hit.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| To read more, re-read with offset=1 and first_line_offset=20."),
            "should have footer re-read advice.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 5: Global character cap hit ──────────────────────────────────

    #[tokio::test]
    async fn global_char_cap_hit() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("cap_hit.txt");

        // 5 lines, each 5 chars of content
        std::fs::write(&file_path, "line1\nline2\nline3\nline4\nline5\n")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        // max_read_chars=14: after "line1"(5)+"line2"(5) = 10 chars consumed + 2 \n = 12
        // "line3" has 5 chars but only 2 chars remaining → truncated to "li"
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            Some(14),
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("1| line1"),
            "line1 should be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("2| line2"),
            "line2 should be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("3| li"),
            "line3 should be truncated to 'li'.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Line 3 above is truncated at character 2."),
            "should have truncation annotation for line 3.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Output capped at 14 characters"),
            "should have global cap annotation.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 6: Offset beyond EOF ─────────────────────────────────────────

    #[tokio::test]
    async fn offset_beyond_eof() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("short.txt");

        // 3 lines
        std::fs::write(&file_path, "line1\nline2\nline3\n")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            Some(10), // start at line 10, but file has only 3 lines
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("#| No content after line 10."),
            "should indicate no content after line 10.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Lines 0-0 of 3 total."),
            "should have zero-lines footer with total=3.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 7: Directory listing ─────────────────────────────────────────

    #[tokio::test]
    async fn directory_listing() -> Result<()> {
        let dir = temp_dir();

        // Create a file and a subdirectory
        std::fs::write(dir.join("alpha.txt"), "content")?;
        std::fs::write(dir.join("beta.rs"), "fn main() {}")?;
        std::fs::create_dir(dir.join("subdir"))?;
        std::fs::write(dir.join("subdir").join("nested.txt"), "nested")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            dir.to_str().unwrap().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        // Directory listing: entries sorted alphabetically
        assert!(
            output.contains("alpha.txt"),
            "should list alpha.txt.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("beta.rs"),
            "should list beta.rs.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("subdir/"),
            "subdir should end with '/'.\nGot:\n{}",
            output
        );
        assert!(
            output.contains(&format!(
                "#| Directory: {} (3 entries)",
                dir.to_str().unwrap()
            )),
            "should have directory footer.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 8: Empty file ────────────────────────────────────────────────

    #[tokio::test]
    async fn empty_file() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("empty.txt");

        // Create an empty file
        std::fs::File::create(&file_path)?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_str().unwrap())),
            "should have header.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Lines 0-0 of 0 total."),
            "should have zero-lines footer.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| File is empty."),
            "should indicate file is empty.\nGot:\n{}",
            output
        );
        assert!(
            !output.contains("No content after line"),
            "empty file should NOT have 'No content after line'.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 9: Hash-pipe in content ──────────────────────────────────────

    #[tokio::test]
    async fn hash_pipe_in_content() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("hashpipe.txt");

        // File content includes lines that LOOK like annotations
        std::fs::write(
            &file_path,
            "#| File: /fake/path\n#| Directory: /fake (5 entries)\nnormal line\n",
        )?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        // The actual header should be present
        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_str().unwrap())),
            "should start with actual header.\nGot:\n{}",
            output
        );

        // The content lines that start with "#| " should have a line-number prefix
        assert!(
            output.contains("1| #| File: /fake/path"),
            "line 1 content with #| should have line prefix '1| '.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("2| #| Directory: /fake (5 entries)"),
            "line 2 content with #| should have line prefix '2| '.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("3| normal line"),
            "line 3 should have line prefix.\nGot:\n{}",
            output
        );

        // The footer should be present
        assert!(
            output.contains("#| Lines 1-3 of 3 total."),
            "should have correct footer.\nGot:\n{}",
            output
        );

        Ok(())
    }

    // ── Test 10: Char cap hit exactly at line boundary ─────────────────────

    #[tokio::test]
    async fn char_cap_boundary() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("boundary.txt");

        // 3 lines, each 5 chars of content + newline
        // max_chars=11: "line1" (5) + \n => chars_consumed=6, remaining=5
        // "line2" (5) fits exactly => chars_consumed=12, remaining=0
        // "line3" has remaining=0 → truncated at boundary (no mid-line truncation)
        std::fs::write(&file_path, "line1\nline2\nline3\n")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            Some(11),
            None,
        );
        let output = collect_stream(stream).await?;

        // First two lines should be present in full
        assert!(
            output.contains("1| line1"),
            "line1 should be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("2| line2"),
            "line2 should be present.\nGot:\n{}",
            output
        );
        // line3 should NOT appear (was truncated between lines)
        assert!(
            !output.contains("3|"),
            "line3 should NOT appear.\nGot:\n{}",
            output
        );
        // Should have "To read more" pointing at the line where truncation occurred
        assert!(
            output.contains("#| To read more, re-read with offset=3 and first_line_offset=0."),
            "should have To read more with offset=3.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Output capped at 11 characters"),
            "should have global cap annotation.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 11: UTF-8 multibyte character handling ────────────────────────

    #[tokio::test]
    async fn utf8_multibyte() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("utf8.txt");

        // Line: "héllo" — 'é' is a 2-byte UTF-8 char
        std::fs::write(&file_path, "héllo\n")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        // first_line_offset=1 skips 'h', showing "éllo" (4 chars)
        // max_read_chars=3 truncates to "éll"
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            Some(3),
            Some(1),
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("1| éll"),
            "should show 'éll' (Unicode correct, not byte-sliced).\nGot:\n{}",
            output
        );
        // chars_start=1 (skipped 'h'), chars_end=1+3=4
        assert!(
            output
                .contains("#| Line 1 above starts at character 1 and is truncated at character 4."),
            "should have correct Unicode character counts.\nGot:\n{}",
            output
        );
        // Per-line "To continue" is not emitted for global char cap truncation.
        // The footer-level "To read more" handles re-read advice instead.
        assert!(
            !output.contains("#| To continue,"),
            "should NOT have per-line 'To continue' for global cap hit.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| To read more, re-read with offset=1 and first_line_offset=4."),
            "should have footer re-read advice.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 12: File without trailing newline ─────────────────────────────

    #[tokio::test]
    async fn no_trailing_newline() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("no_newline.txt");

        // No trailing newline
        std::fs::write(&file_path, "line1\nline2\nline3")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("1| line1"),
            "line1 should be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("2| line2"),
            "line2 should be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("3| line3"),
            "line3 should be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Lines 1-3 of 3 total."),
            "should have correct footer with 3 total lines.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 13: limit parameter restricts line count ──────────────────────

    #[tokio::test]
    async fn limit_parameter() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("many_lines.txt");

        // 10 lines
        let content: String = (1..=10)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&file_path, content)?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            Some(5),
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("1| line1"),
            "line1 should be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("5| line5"),
            "line5 should be present.\nGot:\n{}",
            output
        );
        assert!(
            !output.contains("6|"),
            "line6 should NOT be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Lines 1-5 of 10 total."),
            "should show correct range with total.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 14: offset + limit combined ───────────────────────────────────

    #[tokio::test]
    async fn offset_plus_limit() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("offset_limit.txt");

        // 10 lines
        let content: String = (1..=10)
            .map(|i| format!("line{}", i))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(&file_path, content)?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        // offset=4, limit=3 → lines 4, 5, 6
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            Some(4),
            Some(3),
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            !output.contains("3|"),
            "line3 should NOT be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("4| line4"),
            "line4 should be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("6| line6"),
            "line6 should be present.\nGot:\n{}",
            output
        );
        assert!(
            !output.contains("7|"),
            "line7 should NOT be present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Lines 4-6 of 10 total."),
            "should show correct range.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 15: first_line_offset beyond line length ──────────────────────

    #[tokio::test]
    async fn first_line_offset_beyond() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("flo_beyond.txt");

        // Single line "abc" (3 chars), first_line_offset=10 exceeds length
        std::fs::write(&file_path, "abc\nshort\n")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            None,
            Some(10),
        );
        let output = collect_stream(stream).await?;

        // The first line should be skipped silently (no "1|" output)
        assert!(
            !output.contains("1|"),
            "first line should be skipped when offset exceeds its length.\nGot:\n{}",
            output
        );
        // The second line should appear normally
        assert!(
            output.contains("2| short"),
            "line2 should appear normally.\nGot:\n{}",
            output
        );
        // The footer uses the first actually emitted line, so it reports
        // "Lines 2-2" since line 1 was skipped (offset beyond its length).
        assert!(
            output.contains("#| Lines 2-2 of 2 total."),
            "should show correct footer (first-emitted-line-based range).\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 16: max_read_chars=0 does not panic ───────────────────────────

    #[tokio::test]
    async fn max_read_chars_zero() -> Result<()> {
        let dir = temp_dir();
        let file_path = dir.join("zero_cap.txt");

        std::fs::write(&file_path, "hello\nworld\n")?;

        let ctx = make_ctx_with_read_permission(&dir)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            Some(0),
            None,
        );
        let output = collect_stream(stream).await?;

        // Header must be present
        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_str().unwrap())),
            "header should still appear.\nGot:\n{}",
            output
        );
        // Footer must be present — no panic
        assert!(
            output.contains("#| Lines"),
            "footer should appear without panic.\nGot:\n{}",
            output
        );
        // max_chars is clamped to 1, so we should see some truncated output
        assert!(
            output.contains("#| Output capped"),
            "cap annotation should appear.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── UTF-8 strict-decode matrix (plan Section 6.1) ─────────────────────

    /// Helper: read `file_path` via the read tool with a permission file inside
    /// `tdir`, returning the collected output (or the read error).
    async fn read_with_ctx(
        tdir: &TestDir,
        file_path: &std::path::Path,
        offset: Option<u64>,
        limit: Option<u64>,
        max_read_chars: Option<u64>,
        first_line_offset: Option<u64>,
    ) -> Result<String> {
        let perm = tdir.path().join("permissions.json");
        let ctx = make_ctx_with_read_permission_at(tdir.path(), &perm)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            offset,
            limit,
            max_read_chars,
            first_line_offset,
        );
        collect_stream(stream).await
    }

    // ── Test 17: invalid UTF-8 mid-file fails loudly ───────────────────────

    #[tokio::test]
    async fn invalid_utf8_mid_file_fails() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("invalid.txt");
        std::fs::write(&file_path, b"line 1\nline \xff\xfe 2\n")?;

        let err = read_with_ctx(&tdir, &file_path, None, None, None, None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&file_path.to_str().unwrap().to_string()),
            "error should contain the path.\nGot: {}",
            msg
        );
        assert!(
            msg.contains("line 2"),
            "error should mention the line number.\nGot: {}",
            msg
        );
        Ok(())
    }

    // ── Test 18: multibyte char truncated at EOF fails loudly ──────────────

    #[tokio::test]
    async fn truncated_multibyte_at_eof_fails() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("truncated.txt");
        // 世 (E4 B8 96) cut to 2 bytes; no trailing newline.
        std::fs::write(&file_path, b"hello\nfoo \xe4\xb8")?;

        let err = read_with_ctx(&tdir, &file_path, None, None, None, None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(&file_path.to_str().unwrap().to_string()),
            "error should contain the path.\nGot: {}",
            msg
        );
        assert!(
            msg.contains("line 2"),
            "error should mention the line number.\nGot: {}",
            msg
        );
        Ok(())
    }

    // ── Test 19: multibyte across many buffer boundaries round-trips ───────

    #[tokio::test]
    async fn multibyte_across_buffer_boundary_roundtrips() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("many.txt");
        // 10,000 世 = 30 KiB, crossing many 8 KiB buffer boundaries.
        let content = format!("{}\nsecond line\n", "世".repeat(10_000));
        std::fs::write(&file_path, content)?;

        let output = read_with_ctx(&tdir, &file_path, None, None, Some(1_000_000), None).await?;

        assert_eq!(output.matches('世').count(), 10_000, "all 世 chars present");
        assert!(
            !output.contains('\u{FFFD}'),
            "no replacement chars.\nGot:\n{}",
            output
        );
        assert!(output.contains("2| second line"), "line 2 present");
        Ok(())
    }

    // ── Test 20: multibyte char exactly at the buffer boundary ─────────────

    #[tokio::test]
    async fn multibyte_char_at_exact_buffer_boundary() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("boundary.txt");
        // 8191 'a's then 世 (E4 B8 96 spans bytes 8191-8193), straddling the
        // 8 KiB fill_buf boundary.
        let mut content = vec![b'a'; 8191];
        content.extend_from_slice("世".as_bytes());
        content.extend_from_slice(b"bbb\n");
        std::fs::write(&file_path, content)?;

        let output = read_with_ctx(&tdir, &file_path, None, None, None, None).await?;

        assert!(
            output.contains("世bbb"),
            "世 should be intact followed by bbb.\nGot:\n{}",
            output
        );
        assert!(
            !output.contains('\u{FFFD}'),
            "no replacement chars.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 21: offset + limit with multibyte lines ───────────────────────

    #[tokio::test]
    async fn offset_and_limit_with_multibyte() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("offset.txt");
        std::fs::write(&file_path, "line1\nline2世\nline3世世\nline4\nline5\n")?;

        let output = read_with_ctx(&tdir, &file_path, Some(2), Some(2), None, None).await?;

        assert!(output.contains("2| line2世"), "line 2 byte-exact");
        assert!(output.contains("3| line3世世"), "line 3 byte-exact");
        assert!(!output.contains("1|"), "line 1 must not appear");
        assert!(!output.contains("4|"), "line 4 must not appear");
        assert!(!output.contains("5|"), "line 5 must not appear");
        assert!(
            output.contains("#| Lines 2-3 of 5 total."),
            "footer correct"
        );
        Ok(())
    }

    // ── Test 22: max_read_chars cap cuts at a char boundary ────────────────

    #[tokio::test]
    async fn max_read_chars_cap_cuts_at_char_boundary() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("cap.txt");
        std::fs::write(&file_path, "世".repeat(10))?;

        let output = read_with_ctx(&tdir, &file_path, None, None, Some(7), None).await?;

        assert_eq!(output.matches('世').count(), 7, "exactly 7 chars");
        assert!(
            output.contains("1| 世世世世世世世"),
            "content line is 7 complete 世.\nGot:\n{}",
            output
        );
        assert!(
            !output.contains('\u{FFFD}'),
            "no replacement chars.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Line 1 above is truncated at character 7."),
            "truncation annotation.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 23: CRLF with multibyte ───────────────────────────────────────

    #[tokio::test]
    async fn crlf_multibyte() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("crlf.txt");
        std::fs::write(&file_path, b"\xe4\xb8\x96\r\n\xe4\xb8\x96\r\n")?;

        let output = read_with_ctx(&tdir, &file_path, None, None, None, None).await?;

        // The trailing \r is preserved: the read output must match the file
        // bytes exactly so the edit tool can find exact strings.
        assert!(output.contains("1| 世\r"), "line 1 is 世 with trailing \\r");
        assert!(output.contains("2| 世\r"), "line 2 is 世 with trailing \\r");
        assert!(
            !output.contains('\u{FFFD}'),
            "no replacement chars.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 24: valid file without trailing newline ───────────────────────

    #[tokio::test]
    async fn no_trailing_newline_valid() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("no_newline.txt");
        std::fs::write(&file_path, b"first\nsecond")?;

        let output = read_with_ctx(&tdir, &file_path, None, None, None, None).await?;

        assert!(output.contains("1| first"), "line 1 present");
        assert!(output.contains("2| second"), "line 2 present");
        assert!(
            output.contains("#| Lines 1-2 of 2 total."),
            "footer correct"
        );
        Ok(())
    }

    // ── Test 25: first_line_offset with multibyte ──────────────────────────

    #[tokio::test]
    async fn first_line_offset_with_multibyte() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("flo.txt");
        std::fs::write(&file_path, "世世世")?;

        let output = read_with_ctx(&tdir, &file_path, None, None, None, Some(1)).await?;

        assert!(
            output.contains("1| 世世"),
            "output starts at the 2nd complete 世.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Line 1 above starts at character 1."),
            "offset annotation.\nGot:\n{}",
            output
        );
        assert!(
            !output.contains('\u{FFFD}'),
            "no replacement chars.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 26: empty file ────────────────────────────────────────────────

    #[tokio::test]
    async fn empty_file_ok() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("empty.txt");
        std::fs::write(&file_path, b"")?;

        let output = read_with_ctx(&tdir, &file_path, None, None, None, None).await?;

        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_str().unwrap())),
            "header only.\nGot:\n{}",
            output
        );
        assert!(output.contains("#| File is empty."), "empty marker present");
        assert!(
            output.contains("#| Lines 0-0 of 0 total."),
            "zero-lines footer.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 27: binary content is still detected (before UTF-8 decode) ────

    #[tokio::test]
    async fn binary_file_still_detected() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("binary.txt");
        // "txt" is not a binary extension, so the content check must fire.
        let content: Vec<u8> = (0..64).flat_map(|_| [0u8, 1, 2, 3]).collect();
        std::fs::write(&file_path, content)?;

        let err = read_with_ctx(&tdir, &file_path, None, None, None, None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("binary"),
            "should be a binary-content error.\nGot: {}",
            msg
        );
        assert!(
            !msg.contains("UTF-8"),
            "should NOT be a UTF-8 error.\nGot: {}",
            msg
        );
        Ok(())
    }

    // ── Test 28: char straddling the buffer boundary + cap counts complete ──

    #[tokio::test]
    async fn straddle_at_cap_boundary_counts_complete_chars() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("straddle.txt");
        // 世 spans bytes 8190-8192 (E4 at 8190, B8 at 8191 — end of the first
        // 8 KiB fill_buf — and 96 at 8192). max_read_chars=8191 makes 世 the
        // last char within the cap.
        let mut content = vec![b'a'; 8190];
        content.extend_from_slice("世".as_bytes());
        content.extend_from_slice(b"bbb\nsecond\n");
        std::fs::write(&file_path, content)?;

        let output = read_with_ctx(&tdir, &file_path, None, None, Some(8191), None).await?;

        let body = output
            .lines()
            .find(|l| l.starts_with("1| "))
            .expect("line 1 content should be present")
            .strip_prefix("1| ")
            .expect("content line has prefix");
        assert_eq!(body.chars().count(), 8191, "exactly 8191 chars");
        assert_eq!(body.matches('a').count(), 8190, "8190 a's");
        assert_eq!(body.matches('世').count(), 1, "one complete 世");
        assert!(
            !output.contains('\u{FFFD}'),
            "no replacement chars.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Line 1 above is truncated at character 8191."),
            "truncation annotation present.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| Output capped at 8191 characters"),
            "global cap annotation.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 29: invalid UTF-8 beyond the char cap truncates (same buffer) ──

    #[tokio::test]
    async fn invalid_utf8_beyond_cap_truncates() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("beyond_cap.txt");
        // The \xff is the 17th character of line 1; the cap cuts at character
        // 10, so the invalid byte would never be displayed. The line's \n is
        // in the same buffer, so the complete-line site handles it.
        std::fs::write(&file_path, b"0123456789abcdef\xffghi\nsecond line\n")?;

        let output = read_with_ctx(&tdir, &file_path, None, None, Some(10), None).await?;

        let body = output
            .lines()
            .find(|l| l.starts_with("1| "))
            .expect("line 1 content should be present")
            .strip_prefix("1| ")
            .expect("content line has prefix");
        assert_eq!(
            body, "0123456789",
            "line 1 shows exactly the 10 chars within the cap"
        );
        assert_eq!(body.chars().count(), 10, "exactly 10 chars");
        assert!(
            output.contains("#| Line 1 above is truncated at character 10."),
            "truncation annotation present.\nGot:\n{}",
            output
        );
        // The read continued past the invalid byte instead of failing: line 2
        // is counted (its text can't be displayed — line 1 already exhausted
        // the 10-char budget plus its newline).
        assert!(
            output.contains("#| Lines 1-1 of 2 total."),
            "read continued past the invalid byte.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| To read more, re-read with offset=2 and first_line_offset=0."),
            "continuation advice points at line 2.\nGot:\n{}",
            output
        );
        Ok(())
    }

    // ── Test 30: invalid UTF-8 beyond the char cap truncates (multi-buffer) ─

    #[tokio::test]
    async fn invalid_utf8_beyond_cap_truncates_multibuffer() -> Result<()> {
        let tdir = TestDir::new("read-utf8");
        let file_path = tdir.path().join("beyond_cap_multi.txt");
        // The \xff is the 101st character of line 1 (beyond the cap) and sits
        // at byte 100, past the 8 KiB fill_buf boundary, so the None arm of
        // the newline search handles it.
        let mut content = vec![b'a'; 100];
        content.push(b'\xff');
        content.resize(content.len() + 9000, b'b');
        content.extend_from_slice(b"\nrest\n");
        std::fs::write(&file_path, content)?;

        let output = read_with_ctx(&tdir, &file_path, None, None, Some(100), None).await?;

        let body = output
            .lines()
            .find(|l| l.starts_with("1| "))
            .expect("line 1 content should be present")
            .strip_prefix("1| ")
            .expect("content line has prefix");
        assert_eq!(body.chars().count(), 100, "exactly 100 chars");
        assert_eq!(body.matches('a').count(), 100, "line 1 is 100 a's");
        assert!(
            output.contains("#| Line 1 above is truncated at character 100."),
            "truncation annotation present.\nGot:\n{}",
            output
        );
        // The read continued past the invalid byte instead of failing: line 2
        // is counted (its text can't be displayed — line 1 already exhausted
        // the 100-char budget plus its newline).
        assert!(
            output.contains("#| Lines 1-1 of 2 total."),
            "read continued past the invalid byte.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("#| To read more, re-read with offset=2 and first_line_offset=0."),
            "continuation advice points at line 2.\nGot:\n{}",
            output
        );
        Ok(())
    }

    /// The None-arm FAIL path: an invalid byte WITHIN the display budget on a
    /// multi-buffer line must still fail loudly (only beyond-budget bytes
    /// truncate). `\xff` is at char 51 of line 1, well within the default
    /// 50000-char budget, and the line spans the 8 KiB boundary so the None
    /// arm of the newline search handles it.
    #[tokio::test]
    async fn invalid_utf8_in_window_fails_on_multibuffer_line() -> Result<()> {
        let mut fixture = b"a".repeat(50);
        fixture.push(0xff);
        fixture.extend(b"b".repeat(9000));
        fixture.extend_from_slice(b"\nrest\n");

        let dir = TestDir::new("read-utf8");
        let file_path = dir.path().join("bad.txt");
        std::fs::write(&file_path, &fixture)?;

        let perm_path = dir.path().join("permissions.json");
        let ctx = make_ctx_with_read_permission_at(dir.path(), &perm_path)?;
        let stream = crate::read::read(
            ctx,
            file_path.to_str().unwrap().to_string(),
            None,
            None,
            None,
            None,
        );
        let err = collect_stream(stream).await.unwrap_err();

        let msg = format!("{err:#}");
        assert!(
            msg.contains(file_path.to_str().unwrap()),
            "error must contain the path.\nGot:\n{msg}"
        );
        assert!(
            msg.contains("line 1"),
            "error must name line 1.\nGot:\n{msg}"
        );
        Ok(())
    }

    // ── directory listing: non-UTF-8 entry names are skipped with a count note ──

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_listing_skips_non_utf8_names_with_note() -> Result<()> {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let dir = TestDir::new("read-utf8");
        std::fs::write(dir.path().join("ok.txt"), "hello")?;
        // A real entry with a non-UTF-8 name (\xff \xfe are invalid UTF-8 bytes).
        let bad_name = OsStr::from_bytes(b"bad-\xff\xfe");
        std::fs::write(dir.path().join(bad_name), "x")?;

        let perm_path = dir.path().join("permissions.json");
        let ctx = make_ctx_with_read_permission_at(dir.path(), &perm_path)?;
        let stream = crate::read::read(
            ctx,
            dir.path().to_str().unwrap().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("ok.txt"),
            "UTF-8 entry must be listed.\nGot:\n{}",
            output
        );
        assert!(
            !output.contains("bad-"),
            "non-UTF-8 entry must not appear in the listing.\nGot:\n{}",
            output
        );
        assert!(
            output.contains("1 non-UTF-8 entry name(s) omitted from this listing."),
            "note with the skipped count must be present.\nGot:\n{}",
            output
        );
        Ok(())
    }

    #[tokio::test]
    async fn directory_listing_all_utf8_has_no_omission_note() -> Result<()> {
        let dir = TestDir::new("read-utf8");
        std::fs::write(dir.path().join("ok.txt"), "hello")?;

        let perm_path = dir.path().join("permissions.json");
        let ctx = make_ctx_with_read_permission_at(dir.path(), &perm_path)?;
        let stream = crate::read::read(
            ctx,
            dir.path().to_str().unwrap().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.contains("ok.txt"),
            "entry must be listed.\nGot:\n{}",
            output
        );
        assert!(
            !output.contains("omitted from this listing"),
            "no omission note when everything is UTF-8.\nGot:\n{}",
            output
        );
        Ok(())
    }
}
