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

    /// Build a ToolContext with file_read permission pre-granted for `dir`.
    fn make_ctx_with_read_permission(dir: &std::path::Path) -> Result<ToolContext> {
        let pm = Arc::new(PermissionManager::new(temp_perm_path()));
        let canonical_dir = dir.canonicalize()?;
        let key = PermissionKey {
            tool: "file_read".to_string(),
            key: "path".to_string(),
            value: canonical_dir.to_string_lossy().to_string(),
        };
        pm.resolve(&key, &PermissionDecision::AllowSession, None)?;
        let scoped =
            ScopedPermissionManager::new("read", pm, Arc::new(|| {}), Arc::new(|| {}), None);
        Ok(ToolContext {
            cancel_token: CancellationToken::new(),
            permission: scoped,
            container_config: None,
            media_dir: None,
            supports_media: false,
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
            file_path.to_string_lossy().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        let expected_header = format!("#| File: {}", file_path.to_string_lossy());
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
            file_path.to_string_lossy().to_string(),
            None,
            None,
            Some(100),
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_string_lossy())),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
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
            dir.to_string_lossy().to_string(),
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
                dir.to_string_lossy()
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
            file_path.to_string_lossy().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_string_lossy())),
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
            file_path.to_string_lossy().to_string(),
            None,
            None,
            None,
            None,
        );
        let output = collect_stream(stream).await?;

        // The actual header should be present
        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_string_lossy())),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
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
            file_path.to_string_lossy().to_string(),
            None,
            None,
            Some(0),
            None,
        );
        let output = collect_stream(stream).await?;

        // Header must be present
        assert!(
            output.starts_with(&format!("#| File: {}", file_path.to_string_lossy())),
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
}
