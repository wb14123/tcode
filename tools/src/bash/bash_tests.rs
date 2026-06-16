use super::starts_with_cd;

#[test]
fn detects_bare_cd() {
    assert!(starts_with_cd("cd"));
    assert!(starts_with_cd("  cd"));
}

#[test]
fn detects_cd_with_directory() {
    assert!(starts_with_cd("cd /some/dir"));
    assert!(starts_with_cd("cd /some/dir && cargo build"));
    assert!(starts_with_cd("cd /some/dir; cargo build"));
    assert!(starts_with_cd("cd /some/dir\n cargo build"));
}

#[test]
fn detects_cd_with_tab() {
    assert!(starts_with_cd("cd\t/some/dir"));
}

#[test]
fn detects_cd_with_ampersand() {
    assert!(starts_with_cd("cd /tmp&&ls"));
    assert!(starts_with_cd("cd&& ls")); // degenerate but still cd
}

#[test]
fn ignores_non_cd_commands() {
    assert!(!starts_with_cd("cargo build"));
    assert!(!starts_with_cd("ls -la"));
    assert!(!starts_with_cd("cdk deploy"));
    assert!(!starts_with_cd("cdrom"));
    assert!(!starts_with_cd("echo cd /tmp"));
}

#[test]
fn ignores_empty_and_whitespace() {
    assert!(!starts_with_cd(""));
    assert!(!starts_with_cd("   "));
}

// ── streaming mode e2e tests ──────────────────────────────────────────

mod streaming {
    use anyhow::Result;
    use llm_rs::permission::{
        KEY_COMMAND, KEY_PATH, PermissionKey, PermissionScope, SCOPE_BASH, SCOPE_FILE_WRITE,
        ScopedPermissionManager, WILDCARD_VALUE,
    };
    use llm_rs::tool::{CancellationToken, ToolContext};
    use std::path::{Path, PathBuf};
    use tokio_stream::StreamExt;

    fn test_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/bash_streaming")
    }

    fn temp_dir() -> Result<PathBuf> {
        let dir = test_root().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }

    /// Create a ToolContext with permissions pre-granted for any bash command.
    /// `always_allow` alone is not enough — complex commands (for-loops,
    /// command substitution, etc.) trigger `prompt_complex_command_permission`
    /// which hangs without a real user. Pre-granting the wildcard bypasses this.
    fn ctx() -> ToolContext {
        ctx_with_cancel_token(CancellationToken::new())
    }

    fn ctx_with_cancel_token(cancel_token: CancellationToken) -> ToolContext {
        use std::sync::Arc;
        let pm = Arc::new(llm_rs::permission::PermissionManager::new(
            test_root().join(format!("pm-{}.json", uuid::Uuid::new_v4())),
        ));
        std::fs::create_dir_all(test_root()).expect("failed to create test-tmp dir");
        pm.add_permission(
            PermissionKey {
                tool: SCOPE_BASH.to_string(),
                key: KEY_COMMAND.to_string(),
                value: WILDCARD_VALUE.to_string(),
            },
            PermissionScope::Session,
        )
        .expect("failed to add wildcard permission");
        let cwd = std::env::current_dir()
            .and_then(|path| path.canonicalize())
            .expect("failed to resolve cwd");
        pm.add_permission(
            PermissionKey {
                tool: SCOPE_FILE_WRITE.to_string(),
                key: KEY_PATH.to_string(),
                value: cwd.to_string_lossy().into_owned(),
            },
            PermissionScope::Session,
        )
        .expect("failed to add cwd file-write permission");
        let test_root = test_root()
            .canonicalize()
            .expect("failed to resolve test root");
        pm.add_permission(
            PermissionKey {
                tool: SCOPE_FILE_WRITE.to_string(),
                key: KEY_PATH.to_string(),
                value: test_root.to_string_lossy().into_owned(),
            },
            PermissionScope::Session,
        )
        .expect("failed to add test-root file-write permission");
        let perm = ScopedPermissionManager::new("bash", pm, Arc::new(|| {}), Arc::new(|| {}), None);
        ToolContext {
            cancel_token,
            permission: perm,
            container_config: None,
            media_dir: None,
            supports_media: false,
            llm: None,
            model: None,
        }
    }

    async fn collect(
        mut stream: impl tokio_stream::Stream<Item = Result<String>> + Unpin,
    ) -> (Vec<String>, Option<String>) {
        let mut chunks = Vec::new();
        let mut err = None;
        while let Some(item) = stream.next().await {
            match item {
                Ok(s) => chunks.push(s),
                Err(e) => {
                    err = Some(e.to_string());
                    break;
                }
            }
        }
        (chunks, err)
    }

    // ── 1. Streaming: output arrives in multiple chunks ────────────────

    #[tokio::test]
    async fn streaming_lines_yielded_before_process_exits() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo a; sleep 0.3; echo b".to_string(),
            false,
            None,
            None,
            None,
            None,
            None,
            "streaming test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(joined.contains("stdout| a"), "expected a: {joined}");
        assert!(joined.contains("stdout| b"), "expected b: {joined}");
        assert!(
            joined.contains("exit_code: 0"),
            "expected metadata: {joined}"
        );
        Ok(())
    }

    // ── 2. Filter applied per-line during streaming ────────────────────

    #[tokio::test]
    async fn filter_applied_per_line_during_streaming() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hello; echo world; echo foo".to_string(),
            false,
            None,
            None,
            Some("hello|foo".to_string()),
            None,
            None,
            "filter test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(joined.contains("stdout| hello"), "expected hello: {joined}");
        assert!(joined.contains("stdout| foo"), "expected foo: {joined}");
        assert!(
            !joined.contains("stdout| world"),
            "world filtered: {joined}"
        );
        assert!(
            joined.contains("[filter kept 2/3 lines]"),
            "expected filter marker: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "expected metadata: {joined}"
        );
        Ok(())
    }

    // ── 3. Head stops after N lines with accurate marker ───────────────

    #[tokio::test]
    async fn head_stops_after_n_lines_with_accurate_marker() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo 1; echo 2; echo 3; echo 4; echo 5".to_string(),
            false,
            None,
            None,
            None,
            Some(2),
            None,
            "head test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(joined.contains("stdout| 1"), "line 1: {joined}");
        assert!(joined.contains("stdout| 2"), "line 2: {joined}");
        assert!(!joined.contains("stdout| 3"), "line 3 omitted: {joined}");
        assert!(
            joined.contains("[... later lines omitted by head=2 ...]"),
            "expected head marker: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "expected metadata: {joined}"
        );
        Ok(())
    }

    // ── 4. Char cap truncates with accurate marker ─────────────────────

    #[tokio::test]
    async fn char_cap_truncates_with_accurate_marker() -> Result<()> {
        // Generate ~25,000 chars via 400 lines of 60-digit numbers.
        let cmd = "for i in $(seq 1 400); do printf '%060d\\n' $i; done";
        let stream = crate::bash::bash(
            ctx(),
            cmd.to_string(),
            false,
            None,
            None,
            None,
            None,
            None,
            "char cap test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(
            joined.contains("output truncated by chars_limit=20000"),
            "expected char cap marker: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "expected metadata: {joined}"
        );
        Ok(())
    }

    // ── 5. Tail works via buffering ────────────────────────────────────

    #[tokio::test]
    async fn tail_still_works_via_buffering() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo 1; echo 2; echo 3; echo 4; echo 5".to_string(),
            false,
            None,
            None,
            None,
            None,
            Some(2),
            "tail test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(!joined.contains("stdout| 1"), "line 1 omitted: {joined}");
        assert!(!joined.contains("stdout| 2"), "line 2 omitted: {joined}");
        assert!(!joined.contains("stdout| 3"), "line 3 omitted: {joined}");
        assert!(joined.contains("stdout| 4"), "line 4: {joined}");
        assert!(joined.contains("stdout| 5"), "line 5: {joined}");
        assert!(
            joined.contains("earlier lines omitted by tail=2"),
            "expected tail marker: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "expected metadata: {joined}"
        );
        Ok(())
    }

    // ── 6. Tail + filter collects all lines ────────────────────────────

    #[tokio::test]
    async fn tail_with_filter_collects_all_lines() -> Result<()> {
        // 10 lines via command substitution — filter keeps even digits,
        // tail=2 keeps last 2 matches.
        let stream = crate::bash::bash(
            ctx(),
            "for i in $(seq 1 10); do echo $i; done".to_string(),
            false,
            None,
            None,
            Some("[2468]".to_string()),
            None,
            Some(2),
            "tail+filter test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(!joined.contains("stdout| 2"), "line 2 dropped: {joined}");
        assert!(!joined.contains("stdout| 4"), "line 4 dropped: {joined}");
        assert!(joined.contains("stdout| 6"), "line 6: {joined}");
        assert!(joined.contains("stdout| 8"), "line 8: {joined}");
        assert!(
            joined.contains("[filter kept 4/10 lines]"),
            "expected filter marker: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "expected metadata: {joined}"
        );
        Ok(())
    }

    // ── 7. Timeout yields partial output before error ──────────────────

    #[tokio::test]
    async fn timeout_yields_partial_output_before_error() -> Result<()> {
        // 500ms timeout with a 10-second sleep — "before" should appear,
        // "after" should not.
        let stream = crate::bash::bash(
            ctx(),
            "echo before; sleep 10; echo after".to_string(),
            false,
            Some(500),
            None,
            None,
            None,
            None,
            "timeout test".to_string(),
        );
        // Safety net: if the stream somehow hangs, fail in 15s.
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            collect(Box::pin(stream)).await
        })
        .await;
        let (chunks, err) = match result {
            Ok((c, e)) => (c, e),
            Err(_elapsed) => panic!("collect timed out after 15s — streaming is stuck"),
        };
        let joined = chunks.join("");
        assert!(
            joined.contains("stdout| before"),
            "partial output: {joined}"
        );
        assert!(
            !joined.contains("stdout| after"),
            "after should not appear: {joined}"
        );
        assert!(err.is_none(), "stream should not error, got: {err:?}");
        assert!(
            joined.contains("timed out"),
            "expected timeout message in metadata: {joined}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn timeout_yields_newline_free_output() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "printf before; sleep 10; printf after".to_string(),
            false,
            Some(300),
            None,
            None,
            None,
            None,
            "timeout partial line test".to_string(),
        );

        let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            collect(Box::pin(stream)).await
        })
        .await;
        let (chunks, err) = match result {
            Ok((chunks, err)) => (chunks, err),
            Err(_elapsed) => panic!("collect timed out waiting for timeout result"),
        };
        let joined = chunks.join("");
        assert!(
            joined.contains("stdout| before"),
            "newline-free output before timeout should be returned: {joined}"
        );
        assert!(
            !joined.contains("stdout| after"),
            "output after timeout should not appear: {joined}"
        );
        assert!(
            err.is_none(),
            "timeout should be reported in metadata: {err:?}"
        );
        assert!(
            joined.contains("timed out"),
            "expected timeout message in metadata: {joined}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_kills_child_process_group() -> Result<()> {
        let dir = temp_dir()?;
        let side_effect = dir.join("side_effect");
        let cancel_token = CancellationToken::new();
        let command = format!(
            "printf 'before\n'; sleep 10; printf done > {}",
            shell_quote(&side_effect)
        );
        let stream = crate::bash::bash(
            ctx_with_cancel_token(cancel_token.clone()),
            command,
            false,
            Some(10_000),
            Some(dir.to_string_lossy().into_owned()),
            None,
            None,
            None,
            "cancellation kill test".to_string(),
        );
        let mut stream = Box::pin(stream);
        let item = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await?;
        let Some(first_chunk) = item else {
            anyhow::bail!("stream ended before yielding initial output");
        };
        let first_chunk = first_chunk?;
        assert!(
            first_chunk.contains("stdout| before"),
            "initial output should arrive before cancellation: {first_chunk}"
        );

        cancel_token.cancel();
        let item = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next()).await?;
        let Some(cancelled) = item else {
            anyhow::bail!("stream ended without cancellation error");
        };
        let err = cancelled.expect_err("cancellation should yield an error");
        assert!(
            err.to_string().contains("Command cancelled"),
            "unexpected cancellation error: {err}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !side_effect.exists(),
            "side effect should not happen after cancellation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn execute_path_lets_bash_manage_cancellation() -> Result<()> {
        let dir = temp_dir()?;
        let side_effect = dir.join("execute_side_effect");
        let cancel_token = CancellationToken::new();
        let command = format!(
            "printf 'before\n'; sleep 10; printf done > {}",
            shell_quote(&side_effect)
        );
        let args = serde_json::json!({
            "command": command,
            "timeout": 10000,
            "description": "execute cancellation test"
        })
        .to_string();
        let tool = crate::bash::bash_tool();
        let mut stream = tool.execute(ctx_with_cancel_token(cancel_token.clone()), args);

        let first = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await?;
        let Some(first) = first else {
            anyhow::bail!("tool stream ended before initial output");
        };
        let first_text = first.as_text().expect("expected text output");
        assert!(
            first_text.contains("stdout| before"),
            "initial output should arrive before cancellation: {first_text}"
        );

        cancel_token.cancel();
        let cancelled =
            tokio::time::timeout(std::time::Duration::from_secs(10), stream.next()).await?;
        let Some(cancelled) = cancelled else {
            anyhow::bail!("tool stream ended without cancellation output");
        };
        let cancelled_text = cancelled.as_text().expect("expected cancellation text");
        assert!(
            cancelled_text.contains("Error: Command cancelled"),
            "bash should handle cancellation itself, got: {cancelled_text}"
        );
        assert!(
            !cancelled_text.contains("cancelled by the user"),
            "generic cancellation wrapper should not handle bash cancellation: {cancelled_text}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(
            !side_effect.exists(),
            "side effect should not happen after execute-path cancellation"
        );
        Ok(())
    }

    #[tokio::test]
    async fn pre_cancelled_direct_bash_does_not_spawn_child() -> Result<()> {
        let dir = temp_dir()?;
        let side_effect = dir.join("pre_cancelled_direct");
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();
        let command = format!("printf done > {}; sleep 10", shell_quote(&side_effect));
        let stream = crate::bash::bash(
            ctx_with_cancel_token(cancel_token),
            command,
            false,
            Some(10_000),
            Some(dir.to_string_lossy().into_owned()),
            None,
            None,
            None,
            "pre cancelled direct test".to_string(),
        );

        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(chunks.is_empty(), "no output expected after pre-cancel");
        let err = err.expect("pre-cancel should yield cancellation error");
        assert!(err.contains("Command cancelled"), "unexpected error: {err}");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !side_effect.exists(),
            "pre-cancelled command should not spawn or write side effect"
        );
        Ok(())
    }

    #[tokio::test]
    async fn pre_cancelled_execute_bash_does_not_spawn_child() -> Result<()> {
        let dir = temp_dir()?;
        let side_effect = dir.join("pre_cancelled_execute");
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();
        let command = format!("printf done > {}; sleep 10", shell_quote(&side_effect));
        let args = serde_json::json!({
            "command": command,
            "timeout": 10000,
            "workdir": dir.to_string_lossy().into_owned(),
            "description": "pre cancelled execute test"
        })
        .to_string();
        let tool = crate::bash::bash_tool();
        let mut stream = tool.execute(ctx_with_cancel_token(cancel_token), args);

        let item = stream
            .next()
            .await
            .expect("pre-cancel should yield an item");
        let text = item.as_text().expect("expected text cancellation item");
        assert!(
            text.contains("Error: Command cancelled"),
            "unexpected cancellation output: {text}"
        );
        assert!(
            !text.contains("cancelled by the user"),
            "generic cancellation wrapper should not handle bash pre-cancel: {text}"
        );
        assert!(stream.next().await.is_none());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !side_effect.exists(),
            "pre-cancelled execute command should not spawn or write side effect"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_yields_newline_free_output() -> Result<()> {
        let cancel_token = CancellationToken::new();
        let stream = crate::bash::bash(
            ctx_with_cancel_token(cancel_token.clone()),
            "printf before; sleep 10; printf after".to_string(),
            false,
            Some(10_000),
            None,
            None,
            None,
            None,
            "cancellation partial line test".to_string(),
        );
        let mut stream = Box::pin(stream);
        {
            let next_item = stream.next();
            tokio::pin!(next_item);

            tokio::select! {
                item = &mut next_item => {
                    panic!("stream yielded before cancellation: {item:?}");
                }
                () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            }

            cancel_token.cancel();
            let item =
                tokio::time::timeout(std::time::Duration::from_secs(10), &mut next_item).await?;
            let Some(first_chunk) = item else {
                anyhow::bail!("stream ended before yielding drained partial output");
            };
            let first_chunk = first_chunk?;
            assert!(
                first_chunk.contains("stdout| before"),
                "newline-free output before cancellation should be returned: {first_chunk}"
            );
        }

        let item = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next()).await?;
        let Some(cancelled) = item else {
            anyhow::bail!("stream ended without cancellation error");
        };
        let err = cancelled.expect_err("cancellation should yield an error");
        assert!(
            err.to_string().contains("Command cancelled"),
            "unexpected cancellation error: {err}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn head_limit_does_not_stop_child_side_effect() -> Result<()> {
        let dir = temp_dir()?;
        let side_effect = dir.join("side_effect");
        let command = format!(
            "printf 'one\ntwo\nthree\n'; sleep 0.2; printf done > {}",
            shell_quote(&side_effect)
        );
        let stream = crate::bash::bash(
            ctx(),
            command,
            false,
            Some(5_000),
            Some(dir.to_string_lossy().into_owned()),
            None,
            Some(1),
            None,
            "head side effect test".to_string(),
        );

        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(joined.contains("stdout| one"), "first line: {joined}");
        assert!(
            !joined.contains("stdout| two"),
            "head omitted line: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "expected normal metadata: {joined}"
        );
        assert_eq!(std::fs::read_to_string(side_effect)?, "done");
        Ok(())
    }

    #[tokio::test]
    async fn char_cap_does_not_stop_child_side_effect() -> Result<()> {
        let dir = temp_dir()?;
        let side_effect = dir.join("side_effect");
        let command = format!(
            "for i in $(seq 1 500); do printf '%060d\n' \"$i\"; done; printf done > {}",
            shell_quote(&side_effect)
        );
        let stream = crate::bash::bash(
            ctx(),
            command,
            false,
            Some(5_000),
            Some(dir.to_string_lossy().into_owned()),
            None,
            None,
            None,
            "char cap side effect test".to_string(),
        );

        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(
            joined.contains("output truncated by chars_limit=20000"),
            "expected char cap marker: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "expected normal metadata: {joined}"
        );
        assert_eq!(std::fs::read_to_string(side_effect)?, "done");
        Ok(())
    }

    #[tokio::test]
    async fn dropped_stream_after_head_does_not_stop_child_side_effect() -> Result<()> {
        let dir = temp_dir()?;
        let side_effect = dir.join("side_effect");
        let command = format!(
            "printf 'one\ntwo\nthree\n'; sleep 0.2; printf done > {}",
            shell_quote(&side_effect)
        );
        let stream = crate::bash::bash(
            ctx(),
            command,
            false,
            Some(5_000),
            Some(dir.to_string_lossy().into_owned()),
            None,
            Some(1),
            None,
            "head dropped stream side effect".to_string(),
        );
        let mut stream = Box::pin(stream);
        let item = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await?;
        let Some(first_chunk) = item else {
            anyhow::bail!("stream ended before yielding the head line");
        };
        let first_chunk = first_chunk?;
        assert!(
            first_chunk.contains("stdout| one"),
            "first chunk should contain the head line: {first_chunk}"
        );
        drop(stream);

        for _attempt in 0..20 {
            if side_effect.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            side_effect.exists(),
            "child should finish the side effect after the stream is dropped"
        );
        assert_eq!(std::fs::read_to_string(side_effect)?, "done");
        Ok(())
    }
}
