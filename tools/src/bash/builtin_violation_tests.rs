//! Tests for the builtin-tool auto-review gate.
//!
//! These tests verify that `has_reviewable_keywords` correctly identifies
//! commands containing shell utilities that have built-in alternatives, and
//! that the review gate in the bash tool behaves correctly (skips when no LLM
//! configured, skips when `skip_auto_review` is true, etc.).
//!
//! Integration tests that execute the bash tool use `echo` with no file-path
//! arguments, which classifies as `ReadCommand { paths: [] }` and passes the
//! permission check without requiring any pre-granted permissions.
//!
//! See `bash_tests.rs` for general bash tool tests and `output_processing_tests.rs`
//! for post-processing pipeline tests.

use super::has_reviewable_keywords;

use llm_rs::tool::{CancellationToken, ToolContext};
use tokio_stream::StreamExt;

// ============================================================================
// Keyword detection unit tests
// ============================================================================

#[test]
fn detects_ls() {
    assert!(has_reviewable_keywords("ls"));
    assert!(has_reviewable_keywords("ls -la"));
    assert!(has_reviewable_keywords("ls /tmp"));
}

#[test]
fn detects_find() {
    assert!(has_reviewable_keywords("find . -name '*.rs'"));
    assert!(has_reviewable_keywords("find /tmp -type f"));
}

#[test]
fn detects_grep() {
    assert!(has_reviewable_keywords("grep pattern file.txt"));
    assert!(has_reviewable_keywords("grep -r 'foo' src/"));
}

#[test]
fn detects_rg() {
    assert!(has_reviewable_keywords("rg pattern"));
    assert!(has_reviewable_keywords("rg -l 'TODO' src/"));
}

#[test]
fn detects_cat() {
    assert!(has_reviewable_keywords("cat file.txt"));
    assert!(has_reviewable_keywords("cat /etc/hosts"));
}

#[test]
fn detects_head() {
    assert!(has_reviewable_keywords("head file.txt"));
    assert!(has_reviewable_keywords("head -n 10 data.csv"));
}

#[test]
fn detects_tail() {
    assert!(has_reviewable_keywords("tail file.txt"));
    assert!(has_reviewable_keywords("tail -f /var/log/syslog"));
}

#[test]
fn detects_sed() {
    assert!(has_reviewable_keywords("sed 's/old/new/' file.txt"));
    assert!(has_reviewable_keywords("sed -i 's/foo/bar/g' *.rs"));
}

#[test]
fn detects_awk() {
    assert!(has_reviewable_keywords("awk '{print $1}' data.txt"));
    assert!(has_reviewable_keywords("awk -F: '{print $1}' /etc/passwd"));
}

#[test]
fn detects_echo() {
    assert!(has_reviewable_keywords("echo hello"));
    assert!(has_reviewable_keywords("echo 'some text'"));
}

#[test]
fn detects_keywords_in_pipelines() {
    assert!(has_reviewable_keywords("cargo test 2>&1 | grep FAIL"));
    assert!(has_reviewable_keywords("cargo build 2>&1 | tail -n 30"));
    assert!(has_reviewable_keywords(
        "cat file.txt | grep pattern | head -n 5"
    ));
}

#[test]
fn detects_keywords_in_compound_commands() {
    assert!(has_reviewable_keywords("cargo build && ls target/"));
    assert!(has_reviewable_keywords("git status; echo done"));
}

#[test]
fn detects_2_gt_amp_1() {
    assert!(has_reviewable_keywords("cargo build 2>&1"));
    assert!(has_reviewable_keywords("cargo test 2>&1 | tail -n 30"));
}

#[test]
fn ignores_non_keyword_commands() {
    assert!(!has_reviewable_keywords("cargo build"));
    assert!(!has_reviewable_keywords("cargo test"));
    assert!(!has_reviewable_keywords("git status"));
    assert!(!has_reviewable_keywords("git log --oneline"));
    assert!(!has_reviewable_keywords("npm install"));
    assert!(!has_reviewable_keywords("docker ps"));
    assert!(!has_reviewable_keywords("python script.py"));
    assert!(!has_reviewable_keywords("make"));
    assert!(!has_reviewable_keywords("rustc --version"));
    assert!(!has_reviewable_keywords("which bash"));
    assert!(!has_reviewable_keywords("wc --help"));
    assert!(!has_reviewable_keywords("file /dev/null"));
}

#[test]
fn word_boundary_avoids_false_positives_on_substrings() {
    // "echo" as substring of other words
    assert!(!has_reviewable_keywords("echolocator"));
    // "cat" as substring
    assert!(!has_reviewable_keywords("scatter plot"));
    assert!(!has_reviewable_keywords("concatenate files"));
    // "ls" as substring
    assert!(!has_reviewable_keywords("lsblk"));
    // "head" as substring
    assert!(!has_reviewable_keywords("ahead of time"));
}

#[test]
fn accepts_keywords_in_filenames_as_false_positive() {
    // These are false positives from the word-boundary approach: `\b`
    // treats `-` as a non-word boundary, so `cat-log.txt` matches
    // `\bcat\b`. This is documented as acceptable — the review LLM
    // will correctly respond CONTINUE for these cases.
    assert!(has_reviewable_keywords("cat-log.txt"));
}

// ============================================================================
// Review gate integration tests
// ============================================================================

fn test_ctx() -> ToolContext {
    ToolContext {
        cancel_token: CancellationToken::new(),
        permission: llm_rs::permission::ScopedPermissionManager::always_allow("bash"),
        container_config: None,
        media_dir: None,
        supports_media: false,
        llm: None,
        model: None,
    }
}

/// Verify that `echo` (a reviewable keyword) works when no LLM is
/// configured. The review is silently skipped (fail-open) and the
/// command proceeds normally through the permission check.
#[tokio::test]
async fn no_llm_allows_reviewable_keyword() {
    let ctx = test_ctx();
    let tool = super::bash_tool();

    let args = serde_json::json!({
        "command": "echo hello",
        "description": "echo test"
    })
    .to_string();

    let mut stream = tool.execute(ctx, args);
    let mut output = String::new();
    while let Some(chunk) = stream.next().await {
        if let llm_rs::media::ContentPart::Text(text) = chunk {
            output.push_str(&text);
        }
    }
    assert!(
        output.contains("hello"),
        "expected 'hello' in output, got: {output}"
    );
}

/// When `skip_auto_review: true` is set, the review is bypassed even
/// for commands containing reviewable keywords. The command proceeds
/// normally (subject to the normal permission check).
#[tokio::test]
async fn skip_auto_review_bypasses_review() {
    let ctx = test_ctx();
    let tool = super::bash_tool();

    let args = serde_json::json!({
        "command": "echo skipped",
        "skip_auto_review": true,
        "description": "skip review test"
    })
    .to_string();

    let mut stream = tool.execute(ctx, args);
    let mut output = String::new();
    while let Some(chunk) = stream.next().await {
        if let llm_rs::media::ContentPart::Text(text) = chunk {
            output.push_str(&text);
        }
    }
    assert!(
        output.contains("skipped"),
        "expected 'skipped' in output, got: {output}"
    );
}

/// A command with no reviewable keywords and `llm: None` proceeds
/// without any review intervention. Uses `echo` with `skip_auto_review`
/// which forces the review gate to be skipped entirely, verifying that
/// the skip path works for the case where a non-reviewable command
/// would take.
#[tokio::test]
async fn skip_auto_review_skips_gate_cleanly() {
    let ctx = test_ctx();
    let tool = super::bash_tool();

    // Even with reviewable keywords, skip_auto_review bypasses the gate
    let args = serde_json::json!({
        "command": "echo gate_skipped",
        "skip_auto_review": true,
        "description": "skip gate test"
    })
    .to_string();

    let mut stream = tool.execute(ctx, args);
    let mut output = String::new();
    while let Some(chunk) = stream.next().await {
        if let llm_rs::media::ContentPart::Text(text) = chunk {
            output.push_str(&text);
        }
    }
    assert!(
        output.contains("gate_skipped"),
        "expected 'gate_skipped' in output, got: {output}"
    );
}

/// Without `skip_auto_review`, a reviewable command with no LLM
/// configured proceeds through the fail-open path. This is the
/// normal path for setups without an LLM for review.
#[tokio::test]
async fn reviewable_keyword_no_llm_fail_open() {
    let ctx = test_ctx();
    let tool = super::bash_tool();

    let args = serde_json::json!({
        "command": "echo fail_open",
        "skip_auto_review": false,
        "description": "fail open test"
    })
    .to_string();

    let mut stream = tool.execute(ctx, args);
    let mut output = String::new();
    while let Some(chunk) = stream.next().await {
        if let llm_rs::media::ContentPart::Text(text) = chunk {
            output.push_str(&text);
        }
    }
    assert!(
        output.contains("fail_open"),
        "expected 'fail_open' in output, got: {output}"
    );
}
