//! Unit tests for the bash tool's output post-processing pipeline.
//!
//! Most cases exercise `post_process` directly (a pure helper that takes a
//! `Vec<String>` of already-tagged lines). The few that need a real bash
//! process are gated through the public `bash` tool entry point with an
//! `always_allow` permission scope.

use regex::Regex;

use super::{DEFAULT_MAX_OUTPUT_CHARS, OutputReducer, post_process};

// ---------- helpers ----------

fn re(pat: &str) -> Regex {
    Regex::new(pat).expect("test pattern must compile")
}

fn lines<I: IntoIterator<Item = &'static str>>(it: I) -> Vec<String> {
    it.into_iter().map(|s| s.to_string()).collect()
}

// ---------- 1. filter ----------

#[test]
fn filter_drops_non_matching_and_emits_kept_total_marker() {
    let input = lines([
        "stdout| info: starting",
        "stderr| error: bad thing",
        "stdout| info: continuing",
        "stderr| error: another bad thing",
        "stdout| info: done",
    ]);
    let out = post_process(
        input,
        Some(&re("error")),
        None,
        None,
        DEFAULT_MAX_OUTPUT_CHARS,
    );
    assert!(
        out.contains("stderr| error: bad thing"),
        "kept lines must appear: {out}"
    );
    assert!(
        out.contains("stderr| error: another bad thing"),
        "kept lines must appear: {out}"
    );
    assert!(
        !out.contains("starting"),
        "non-matching lines must be dropped: {out}"
    );
    assert!(
        out.contains("[filter kept 2/5 lines]"),
        "filter marker must report real counts: {out}"
    );
}

#[test]
fn filter_marker_omitted_when_all_lines_kept() {
    let input = lines(["stdout| a", "stdout| b", "stdout| c"]);
    let out = post_process(input, Some(&re(".")), None, None, DEFAULT_MAX_OUTPUT_CHARS);
    assert!(out.contains("stdout| a"));
    assert!(out.contains("stdout| c"));
    assert!(
        !out.contains("[filter"),
        "no filter marker when nothing dropped: {out}"
    );
}

#[test]
fn filter_zero_match_emits_dedicated_marker() {
    let input = lines(["stdout| a", "stdout| b", "stdout| c"]);
    let out = post_process(
        input,
        Some(&re("nope")),
        None,
        None,
        DEFAULT_MAX_OUTPUT_CHARS,
    );
    assert!(
        out.contains("[filter matched 0/3 lines — command produced output but none matched]"),
        "zero-match variant must be emitted: {out}"
    );
    assert!(
        !out.contains("[filter kept"),
        "zero-match marker should replace kept marker: {out}"
    );
}

#[test]
fn filter_marker_not_emitted_when_input_was_empty() {
    // Edge case: total == 0 → kept_count == total → no marker.
    let out = post_process(vec![], Some(&re(".")), None, None, DEFAULT_MAX_OUTPUT_CHARS);
    assert_eq!(out, "");
}

// ---------- 2. head ----------

#[test]
fn head_keeps_first_n_and_emits_bottom_marker() {
    let input = lines([
        "stdout| 1",
        "stdout| 2",
        "stdout| 3",
        "stdout| 4",
        "stdout| 5",
    ]);
    let out = post_process(input, None, Some(2), None, DEFAULT_MAX_OUTPUT_CHARS);
    assert!(out.starts_with("stdout| 1\nstdout| 2"), "got: {out}");
    assert!(
        out.contains("[... later lines omitted by head=2 ...]"),
        "bottom marker must report head truncation: {out}"
    );
    // No tail marker.
    assert!(!out.contains("earlier lines omitted"));
}

#[test]
fn head_no_marker_when_input_fits() {
    let input = lines(["stdout| 1", "stdout| 2"]);
    let out = post_process(input, None, Some(5), None, DEFAULT_MAX_OUTPUT_CHARS);
    assert_eq!(out, "stdout| 1\nstdout| 2");
}

// ---------- 3. tail ----------

#[test]
fn tail_keeps_last_n_and_emits_top_marker() {
    let input = lines([
        "stdout| 1",
        "stdout| 2",
        "stdout| 3",
        "stdout| 4",
        "stdout| 5",
    ]);
    let out = post_process(input, None, None, Some(2), DEFAULT_MAX_OUTPUT_CHARS);
    let expected_top = "[... earlier lines omitted by tail=2 ...]";
    assert!(out.starts_with(expected_top), "got: {out}");
    assert!(out.ends_with("stdout| 4\nstdout| 5"), "got: {out}");
    // No head marker.
    assert!(!out.contains("later lines omitted"));
}

#[test]
fn tail_no_marker_when_input_fits() {
    // Symmetric to head_no_marker_when_input_fits — tail=N where N >= total
    // must return the input unchanged with no top marker.
    let input = lines(["stdout| 1", "stdout| 2"]);
    let out = post_process(input, None, None, Some(5), DEFAULT_MAX_OUTPUT_CHARS);
    assert_eq!(out, "stdout| 1\nstdout| 2");
    assert!(!out.contains("earlier lines omitted"));
}

// ---------- 4. filter + tail composition ----------

#[test]
fn filter_then_tail_only_sees_matching_lines() {
    // 5 matching, 5 non-matching → after filter: 5 → tail=2 keeps 2 of those 5.
    let input = lines([
        "stdout| info 1",
        "stderr| error 1",
        "stdout| info 2",
        "stderr| error 2",
        "stdout| info 3",
        "stderr| error 3",
        "stdout| info 4",
        "stderr| error 4",
        "stdout| info 5",
        "stderr| error 5",
    ]);
    let out = post_process(
        input,
        Some(&re("error")),
        None,
        Some(2),
        DEFAULT_MAX_OUTPUT_CHARS,
    );
    assert!(
        out.contains("stderr| error 4"),
        "tail must keep last 2 matches: {out}"
    );
    assert!(
        out.contains("stderr| error 5"),
        "tail must keep last 2 matches: {out}"
    );
    assert!(
        !out.contains("stderr| error 3"),
        "tail must drop earlier matches: {out}"
    );
    // Top marker reports that earlier matching lines were dropped.
    assert!(
        out.contains("[... earlier lines omitted by tail=2 ...]"),
        "top marker counts post-filter drops: {out}"
    );
    // Filter marker says 5/10 (real total).
    assert!(
        out.contains("[filter kept 5/10 lines]"),
        "filter marker counts the original total: {out}"
    );
}

// ---------- 8. char-cap truncation ----------

#[test]
fn char_cap_cuts_at_last_newline_and_appends_marker() {
    // 10 lines x 5 chars + 9 newlines = 59 chars total. Cap at 12 keeps
    // first 2 lines (5 + 1 + 5 = 11), then appends a simple truncation marker.
    let input = lines([
        "abcde", "fghij", "klmno", "pqrst", "uvwxy", "zabcd", "efghi", "jklmn", "opqrs", "tuvwx",
    ]);
    let out = post_process(input, None, None, None, 12);
    assert!(
        out.starts_with("abcde\nfghij"),
        "kept lines must come first: {out}"
    );
    assert!(
        out.contains("[... output truncated by chars_limit=12 ...]"),
        "chars-limit marker must report truncation: {out}"
    );
    // The cut must land on a newline boundary — no partial line in the
    // content portion.
    let content_end = out.find("\n[...").expect("marker on its own line");
    let content = &out[..content_end];
    assert!(
        !content.ends_with("\n"),
        "content must not have a trailing newline before marker: {content:?}"
    );
}

#[test]
fn char_cap_no_marker_when_under_cap() {
    let input = lines(["one", "two", "three"]);
    let out = post_process(input, None, None, None, DEFAULT_MAX_OUTPUT_CHARS);
    assert_eq!(out, "one\ntwo\nthree");
    assert!(!out.contains("chars_limit"));
}

// ---------- 9. markers do not count toward the char cap ----------

#[test]
fn markers_excluded_from_char_cap_so_content_still_shows() {
    // Cap small enough that, if markers counted, the marker alone would
    // crowd out content. Verify that at least some real content survives.
    let input = lines([
        "stdout| line one is here",
        "stdout| line two is here",
        "stdout| line three is here",
        "stdout| line four is here",
        "stdout| line five is here",
    ]);
    // Cap = 25 chars. The chars-limit marker alone is much longer than 25.
    let out = post_process(input, None, None, None, 25);
    assert!(
        out.contains("stdout| line one is here"),
        "first content line must survive: {out}"
    );
    assert!(
        out.contains("[... output truncated by chars_limit="),
        "marker must still be appended: {out}"
    );
    // Marker on its own line — content portion (before marker) must end at
    // a newline boundary.
    let pre_marker = out
        .split("\n[... output truncated by chars_limit=")
        .next()
        .expect("split must yield content");
    assert!(!pre_marker.is_empty(), "content portion must not be empty");
}

// ---------- 11. ^stderr\| filter isolates stderr lines ----------

#[test]
fn stderr_prefix_filter_isolates_stderr_lines() {
    // The post-processor sees already-tagged input (read_process_output adds
    // the prefix). Verify that a regex matching the prefix correctly drops
    // stdout lines and keeps stderr lines.
    let input = lines([
        "stdout| info: starting",
        "stderr| warn: foo",
        "stdout| info: continuing",
        "stderr| error: bar",
        "stdout| info: done",
    ]);
    let out = post_process(
        input,
        Some(&re(r"^stderr\|")),
        None,
        None,
        DEFAULT_MAX_OUTPUT_CHARS,
    );
    assert!(out.contains("stderr| warn: foo"));
    assert!(out.contains("stderr| error: bar"));
    assert!(!out.contains("stdout|"));
    assert!(out.contains("[filter kept 2/5 lines]"));
}

// ---------- 12. no params → identity (modulo char cap) ----------

#[test]
fn no_params_passes_lines_through_unchanged() {
    let input = lines(["stdout| a", "stderr| b", "stdout| c"]);
    let out = post_process(input, None, None, None, DEFAULT_MAX_OUTPUT_CHARS);
    assert_eq!(out, "stdout| a\nstderr| b\nstdout| c");
}

// ---------- combined: filter + tail + char cap ----------

#[test]
fn composed_filter_tail_and_char_cap_emits_all_relevant_markers() {
    // 6 lines: 4 stderr (matching), 2 stdout (non-matching).
    // filter → 4 lines; tail=3 → drops 1 from top; cap=18 chars → drops more.
    let input = lines([
        "stdout| info hello",
        "stderr| err one", // 14 chars
        "stdout| info world",
        "stderr| err two",   // 14 chars
        "stderr| err three", // 16 chars
        "stderr| err four",  // 15 chars
    ]);
    let out = post_process(input, Some(&re("^stderr")), None, Some(3), 18);
    // Top marker: earlier matching line dropped by tail.
    assert!(
        out.contains("[... earlier lines omitted by tail=3 ...]"),
        "top marker missing: {out}"
    );
    // Filter marker: 4 kept of 6 total.
    assert!(
        out.contains("[filter kept 4/6 lines]"),
        "filter marker missing: {out}"
    );
    // Chars-limit marker present (cap is tighter than 3 lines combined).
    assert!(
        out.contains("chars_limit=18"),
        "chars-limit marker missing: {out}"
    );
}

#[test]
fn tail_char_cap_preserves_newest_lines() {
    let input = lines([
        "stdout| old one",
        "stdout| old two",
        "stdout| middle three",
        "stdout| almost last",
        "stdout| final",
    ]);
    let out = post_process(input, None, None, Some(4), 14);
    assert!(
        out.contains("stdout| final"),
        "tail plus char cap must preserve newest line: {out}"
    );
    assert!(
        !out.contains("stdout| middle three"),
        "older retained tail lines should be dropped first by char cap: {out}"
    );
    assert!(
        out.contains("[... earlier lines omitted by tail=4 ...]"),
        "tail marker missing: {out}"
    );
    assert!(
        out.contains("[... output truncated by chars_limit=14 ...]"),
        "char cap marker missing: {out}"
    );
}

// ---------- had_truncation() tests ----------

#[test]
fn had_truncation_false_for_no_truncation() {
    let mut reducer = OutputReducer::new(None, None, None, DEFAULT_MAX_OUTPUT_CHARS);
    for line in lines(["stdout| hello", "stdout| world"]) {
        reducer.feed(line);
    }
    reducer.final_output();
    assert!(!reducer.had_truncation());
}

#[test]
fn had_truncation_true_for_char_cap() {
    let small_cap = 10;
    let mut reducer = OutputReducer::new(None, None, None, small_cap);
    // Feed a line longer than the cap — it should hit the cap.
    for line in lines(["stdout| this is a very long line that exceeds the cap"]) {
        reducer.feed(line);
    }
    reducer.final_output();
    assert!(reducer.had_truncation());
}

#[test]
fn had_truncation_true_for_head() {
    let mut reducer = OutputReducer::new(None, Some(1), None, DEFAULT_MAX_OUTPUT_CHARS);
    for line in lines(["stdout| first", "stdout| second", "stdout| third"]) {
        reducer.feed(line);
    }
    reducer.final_output();
    assert!(reducer.had_truncation());
}

#[test]
fn had_truncation_true_for_tail() {
    let mut reducer = OutputReducer::new(None, None, Some(2), DEFAULT_MAX_OUTPUT_CHARS);
    for line in lines([
        "stdout| one",
        "stdout| two",
        "stdout| three",
        "stdout| four",
        "stdout| five",
    ]) {
        reducer.feed(line);
    }
    reducer.final_output();
    assert!(reducer.had_truncation());
}

#[test]
fn had_truncation_true_for_filter_dropping_lines() {
    let mut reducer = OutputReducer::new(Some(re("error")), None, None, DEFAULT_MAX_OUTPUT_CHARS);
    for line in lines(["stdout| info one", "stderr| error one", "stdout| info two"]) {
        reducer.feed(line);
    }
    reducer.final_output();
    assert!(reducer.had_truncation());
}

#[test]
fn had_truncation_true_for_multiple_truncations() {
    // Char cap + head: both trigger truncation.
    let mut reducer = OutputReducer::new(None, Some(1), None, 10);
    for line in lines(["stdout| first", "stdout| second", "stdout| third"]) {
        reducer.feed(line);
    }
    reducer.final_output();
    assert!(reducer.had_truncation());
}

// ---------- end-to-end (validation rejection) tests ----------
//
// These exercise the public `bash` tool entry point. They cover validation
// errors that fire BEFORE any process spawns, so they need no permission
// grants — `always_allow` works because the validation error is yielded
// before the permission check would run.

mod e2e {
    use anyhow::Result;
    use llm_rs::permission::ScopedPermissionManager;
    use llm_rs::tool::{CancellationToken, ToolContext};
    use tokio_stream::StreamExt;

    fn ctx() -> ToolContext {
        ToolContext {
            cancel_token: CancellationToken::new(),
            permission: ScopedPermissionManager::always_allow("bash"),
            container_config: None,
            session_dir: None,
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

    // 5. head + tail mutually exclusive
    #[tokio::test]
    async fn head_and_tail_both_set_is_validation_error() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hi".to_string(),
            false,
            None,
            None,
            None,
            Some(1),
            Some(1),
            "test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(chunks.is_empty(), "no output expected on validation error");
        let err = err.expect("validation error expected");
        assert!(
            err.contains("'head' and 'tail' are mutually exclusive"),
            "got: {err}"
        );
        Ok(())
    }

    // 6. invalid filter regex
    #[tokio::test]
    async fn invalid_filter_regex_is_validation_error() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hi".to_string(),
            false,
            None,
            None,
            Some("[unclosed".to_string()),
            None,
            None,
            "test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(chunks.is_empty(), "no output expected on validation error");
        let err = err.expect("validation error expected");
        assert!(
            err.contains("invalid 'filter' regex"),
            "error must mention filter: {err}"
        );
        Ok(())
    }

    // 7a. head = 0 is normalized to None (not an error) — some models (OpenAI)
    // send 0 for unused optional integer params instead of omitting them.
    #[tokio::test]
    async fn head_zero_is_treated_as_none() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hi".to_string(),
            false,
            None,
            None,
            None,
            Some(0),
            None,
            "test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "head=0 should not be an error, got: {err:?}");
        assert!(!chunks.is_empty(), "should produce output");
        Ok(())
    }

    // 7b. tail = 0 is normalized to None (not an error).
    #[tokio::test]
    async fn tail_zero_is_treated_as_none() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hi".to_string(),
            false,
            None,
            None,
            None,
            None,
            Some(0),
            "test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "tail=0 should not be an error, got: {err:?}");
        assert!(!chunks.is_empty(), "should produce output");
        Ok(())
    }

    // 7c. timeout = 0 is normalized to None (not an immediate timeout).
    #[tokio::test]
    async fn timeout_zero_is_treated_as_none() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hi".to_string(),
            false,
            Some(0),
            None,
            None,
            None,
            None,
            "test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(
            err.is_none(),
            "timeout=0 should not be an error, got: {err:?}"
        );
        let joined = chunks.join("");
        assert!(
            joined.contains("stdout| hi"),
            "should produce output: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "should finish normally: {joined}"
        );
        Ok(())
    }

    // 7d. head=0 + tail=0 both normalize to None, so no mutual-exclusion error.
    #[tokio::test]
    async fn head_zero_tail_zero_is_treated_as_none() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hi".to_string(),
            false,
            None,
            None,
            None,
            Some(0),
            Some(0),
            "test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(
            err.is_none(),
            "head=0 + tail=0 should not be an error, got: {err:?}"
        );
        assert!(!chunks.is_empty(), "should produce output");
        Ok(())
    }

    // 7e. head=N + tail=0 normalizes to just head=N (no mutual-exclusion error).
    #[tokio::test]
    async fn head_nonzero_tail_zero_is_treated_as_head_only() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hi".to_string(),
            false,
            None,
            None,
            None,
            Some(2),
            Some(0),
            "test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(
            err.is_none(),
            "head=2 + tail=0 should not be an error, got: {err:?}"
        );
        assert!(!chunks.is_empty(), "should produce output");
        Ok(())
    }

    // 10/13. End-to-end smoke test: `echo` runs, output is `stdout| ` tagged
    // and the metadata trailer is appended unchanged. `echo hi` is classified
    // as ReadCommand with no path arguments, so it passes the permission
    // check without any pre-grant.
    #[tokio::test]
    async fn echo_output_is_stdout_tagged_end_to_end() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo hello".to_string(),
            false,
            None,
            None,
            None,
            None,
            None,
            "echo smoke test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "echo should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(
            joined.contains("stdout| hello"),
            "stdout must be prefix-tagged: {joined}"
        );
        assert!(
            joined.contains("<bash_metadata>") && joined.contains("exit_code: 0"),
            "metadata trailer must still be appended: {joined}"
        );
        // No truncation or filter markers expected for this small output.
        assert!(!joined.contains("[..."), "no markers expected: {joined}");
        Ok(())
    }

    // 10. End-to-end: real process emits both stdout and stderr; verify
    // both streams are tagged at source by `read_process_output`. This is
    // the only test that exercises the live `lines_stream` → `STDERR_TAG`
    // wiring — a regression there would not be caught by the pure-helper
    // tests, which feed already-tagged input. `>&2` is an fd redirect
    // (verified in command_parser_tests), not a file redirect, so the
    // compound command decomposes into two ReadCommands with empty paths
    // and passes the permission check without any pre-grant.
    #[tokio::test]
    async fn stdout_and_stderr_both_tagged_end_to_end() -> Result<()> {
        let stream = crate::bash::bash(
            ctx(),
            "echo out; echo err 1>&2".to_string(),
            false,
            None,
            None,
            None,
            None,
            None,
            "stderr smoke test".to_string(),
        );
        let (chunks, err) = collect(Box::pin(stream)).await;
        assert!(err.is_none(), "command should succeed: {err:?}");
        let joined = chunks.join("");
        assert!(
            joined.contains("stdout| out"),
            "stdout line must be tagged `stdout| `: {joined}"
        );
        assert!(
            joined.contains("stderr| err"),
            "stderr line must be tagged `stderr| `: {joined}"
        );
        assert!(
            joined.contains("exit_code: 0"),
            "metadata trailer must still be appended: {joined}"
        );
        Ok(())
    }
}
