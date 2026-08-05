use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use llm_rs::conversation::{BroadcastMessage, ConversationState, Message, MessageEndStatus};
use llm_rs::llm::{ChatOptions, LLMMessage};
use llm_rs::media::{ContentPart, MediaData};

use super::branch::{
    DisplayCut, build_branch_content, collect_media_refs_from_display,
    collect_media_refs_from_state, collect_subagent_ids, collect_tool_call_ids,
    commit_branch_staging, copy_dir_recursive, shell_quote, truncate_display_at_msg_id,
    truncate_state_at_user, validate_branch,
};

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/branch")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

// ---------------------------------------------------------------------------
// Fixture builders
// ---------------------------------------------------------------------------

fn envelope(id: i32, msg: Message) -> BroadcastMessage {
    BroadcastMessage { id, msg }
}

fn user_envelope(id: i32, content: &str, media: &[&str]) -> BroadcastMessage {
    envelope(
        id,
        Message::UserMessage {
            created_at: 1,
            content: Arc::new(content.to_string()),
            media_filenames: media.iter().map(|s| s.to_string()).collect(),
        },
    )
}

fn assistant_start_envelope(id: i32) -> BroadcastMessage {
    envelope(id, Message::AssistantMessageStart { created_at: 1 })
}

/// Write `envelopes` followed by `extra_lines` (raw, one per element) as a
/// fixture `display.jsonl` in `dir`. Returns the file path.
fn write_display(dir: &Path, envelopes: &[BroadcastMessage], extra_lines: &[&str]) -> PathBuf {
    let path = dir.join("display.jsonl");
    let mut content = String::new();
    for env in envelopes {
        content.push_str(&serde_json::to_string(env).expect("envelope serializes to JSON"));
        content.push('\n');
    }
    for line in extra_lines {
        content.push_str(line);
        content.push('\n');
    }
    std::fs::write(&path, content).expect("failed to write fixture display.jsonl");
    path
}

fn read_lines(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    Ok(content.lines().map(str::to_string).collect())
}

fn make_state(llm_msgs: Vec<LLMMessage>) -> ConversationState {
    ConversationState {
        id: "conv-1".to_string(),
        model: "test-model".to_string(),
        llm_msgs,
        chat_options: ChatOptions::default(),
        msg_id_counter: 100,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_creation_tokens: 0,
        total_cache_read_tokens: 0,
        aggregate_input_tokens: 0,
        aggregate_output_tokens: 0,
        aggregate_cache_creation_tokens: 0,
        aggregate_cache_read_tokens: 0,
        single_turn: false,
        subagent_depth: 0,
    }
}

fn user_msg(text: &str, media: &[&str]) -> LLMMessage {
    let mut parts = vec![ContentPart::Text(text.to_string())];
    for rel in media {
        parts.push(ContentPart::Media(MediaData::new(
            rel.to_string(),
            "image/png".to_string(),
        )));
    }
    LLMMessage::User(parts)
}

fn assistant_msg(content: &str) -> LLMMessage {
    LLMMessage::Assistant {
        content: content.to_string(),
        tool_calls: vec![],
        raw: None,
    }
}

fn make_cut(
    ordinal: usize,
    content: &str,
    media: &[&str],
    retained_ids: Vec<i32>,
    target_id: i32,
) -> DisplayCut {
    let max_retained_id = retained_ids.iter().copied().max();
    DisplayCut {
        retained_lines: vec![],
        retained_envelopes: vec![],
        retained_ids,
        max_retained_id,
        target_ordinal: ordinal,
        target: Message::UserMessage {
            created_at: 1,
            content: Arc::new(content.to_string()),
            media_filenames: media.iter().map(|s| s.to_string()).collect(),
        },
        target_id,
    }
}

// ---------------------------------------------------------------------------
// truncate_state_at_user
// ---------------------------------------------------------------------------

fn state_with_three_users() -> ConversationState {
    make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("one", &[]),
        assistant_msg("a1"),
        user_msg("two", &[]),
        user_msg("three", &[]),
    ])
}

#[test]
fn truncate_state_keeps_prefix_before_middle_ordinal() -> Result<()> {
    let state = state_with_three_users();
    let truncated = truncate_state_at_user(state, 2)?;
    assert_eq!(truncated.llm_msgs.len(), 3);
    assert!(matches!(truncated.llm_msgs[0], LLMMessage::System(_)));
    match &truncated.llm_msgs[1] {
        LLMMessage::User(parts) => {
            assert_eq!(parts[0].as_text(), Some("one"));
        }
        other => panic!("expected User, got {other:?}"),
    }
    Ok(())
}

#[test]
fn truncate_state_ordinal_one_keeps_only_system() -> Result<()> {
    let state = state_with_three_users();
    let truncated = truncate_state_at_user(state, 1)?;
    assert_eq!(truncated.llm_msgs.len(), 1);
    assert!(matches!(truncated.llm_msgs[0], LLMMessage::System(_)));
    Ok(())
}

#[test]
fn truncate_state_ordinal_zero_errors() {
    let state = state_with_three_users();
    let err = truncate_state_at_user(state, 0).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("ordinal 0"), "unexpected error: {msg}");
    assert!(msg.contains("user message"), "unexpected error: {msg}");
}

#[test]
fn truncate_state_ordinal_beyond_count_errors() {
    let state = state_with_three_users();
    let err = truncate_state_at_user(state, 4).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("ordinal 4"), "unexpected error: {msg}");
    assert!(
        msg.contains("3"),
        "expected the user count in the error: {msg}"
    );
}

// ---------------------------------------------------------------------------
// truncate_display_at_msg_id
// ---------------------------------------------------------------------------

#[test]
fn truncate_display_happy_path() -> Result<()> {
    let dir = temp_dir();
    let e1 = user_envelope(1, "hello", &[]);
    let e2 = assistant_start_envelope(2);
    let e3 = user_envelope(3, "second", &[]);
    let l1 = serde_json::to_string(&e1).expect("serialize");
    let l2 = serde_json::to_string(&e2).expect("serialize");
    let path = write_display(&dir, &[e1, e2, e3], &[]);
    let lines = read_lines(&path)?;

    let cut = truncate_display_at_msg_id(&lines, 3)?;
    assert_eq!(cut.target_ordinal, 2);
    assert_eq!(cut.retained_lines, vec![l1, l2]);
    assert_eq!(cut.retained_ids, vec![1, 2]);
    assert_eq!(cut.max_retained_id, Some(2));
    assert_eq!(cut.target_id, 3);
    match &cut.target {
        Message::UserMessage { content, .. } => {
            assert_eq!(content.as_str(), "second");
        }
        other => panic!("expected UserMessage, got {other:?}"),
    }
    Ok(())
}

#[test]
fn truncate_display_target_not_found_errors() -> Result<()> {
    let dir = temp_dir();
    let path = write_display(&dir, &[user_envelope(1, "hello", &[])], &[]);
    let lines = read_lines(&path)?;
    let err = truncate_display_at_msg_id(&lines, 99).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("99"), "unexpected error: {msg}");
    assert!(msg.contains("not found"), "unexpected error: {msg}");
    Ok(())
}

#[test]
fn truncate_display_legacy_line_errors_with_old_format() -> Result<()> {
    let dir = temp_dir();
    let legacy = r#"{"UserMessage": {"msg_id": 3, "content": "hi", "created_at": 1}}"#;
    let path = write_display(&dir, &[], &[legacy]);
    let lines = read_lines(&path)?;
    let err = truncate_display_at_msg_id(&lines, 3).unwrap_err();
    assert!(
        err.to_string().contains("old format"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn truncate_display_mixed_new_then_legacy_errors() -> Result<()> {
    let dir = temp_dir();
    let legacy = r#"{"UserMessage": {"msg_id": 2, "content": "legacy", "created_at": 1}}"#;
    let target_line = serde_json::to_string(&user_envelope(3, "third", &[])).expect("serialize");
    let path = write_display(
        &dir,
        &[user_envelope(1, "hello", &[])],
        &[legacy, &target_line],
    );
    let lines = read_lines(&path)?;
    let err = truncate_display_at_msg_id(&lines, 3).unwrap_err();
    assert!(
        err.to_string().contains("old format"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn truncate_display_skips_trailing_partial_line() -> Result<()> {
    let dir = temp_dir();
    let e1 = user_envelope(1, "hello", &[]);
    let e3 = user_envelope(3, "third", &[]);
    let l1 = serde_json::to_string(&e1).expect("serialize");
    let partial = r#"{"id": 9, "msg": {"Assis"#;
    let target_line = serde_json::to_string(&e3).expect("serialize");
    let path = write_display(&dir, &[e1], &[partial, &target_line]);
    let lines = read_lines(&path)?;

    let cut = truncate_display_at_msg_id(&lines, 3)?;
    assert_eq!(cut.target_ordinal, 2);
    assert_eq!(cut.retained_lines, vec![l1, partial.to_string()]);
    assert_eq!(cut.retained_ids, vec![1]);
    assert_eq!(cut.retained_envelopes.len(), 1);
    Ok(())
}

#[test]
fn truncate_display_empty_prefix_when_target_is_first_line() -> Result<()> {
    let dir = temp_dir();
    let path = write_display(&dir, &[user_envelope(1, "first", &[])], &[]);
    let lines = read_lines(&path)?;

    let cut = truncate_display_at_msg_id(&lines, 1)?;
    assert_eq!(cut.target_ordinal, 1);
    assert!(cut.retained_lines.is_empty());
    assert!(cut.retained_envelopes.is_empty());
    assert!(cut.retained_ids.is_empty());
    assert_eq!(cut.max_retained_id, None);
    Ok(())
}

#[test]
fn truncate_display_skips_lines_without_any_id() -> Result<()> {
    let dir = temp_dir();
    let target_line = serde_json::to_string(&user_envelope(2, "second", &[])).expect("serialize");
    let path = write_display(&dir, &[], &[r#"{"foo": {"bar": 1}}"#, &target_line]);
    let lines = read_lines(&path)?;

    let cut = truncate_display_at_msg_id(&lines, 2)?;
    assert_eq!(cut.target_ordinal, 1);
    assert!(cut.retained_ids.is_empty());
    assert_eq!(cut.retained_lines.len(), 1);
    Ok(())
}

// ---------------------------------------------------------------------------
// validate_branch
// ---------------------------------------------------------------------------

#[test]
fn validate_branch_passes_on_consistent_fixture() -> Result<()> {
    let state = make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("hello", &["a.png"]),
    ]);
    let cut = make_cut(1, "hello", &["a.png"], vec![], 1);
    validate_branch(
        &state,
        &cut,
        Path::new("state.json"),
        Path::new("display.jsonl"),
    )?;
    Ok(())
}

#[test]
fn validate_branch_fires_on_content_mismatch() {
    let state = make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("world", &[]),
    ]);
    let cut = make_cut(1, "hello", &[], vec![], 1);
    let err = validate_branch(
        &state,
        &cut,
        Path::new("state.json"),
        Path::new("display.jsonl"),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("content"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_branch_fires_on_media_filenames_mismatch() {
    let state = make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("hello", &["a.png"]),
    ]);
    let cut = make_cut(1, "hello", &[], vec![], 1);
    let err = validate_branch(
        &state,
        &cut,
        Path::new("state.json"),
        Path::new("display.jsonl"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("media"), "unexpected error: {err}");
}

#[test]
fn validate_branch_fires_on_out_of_order_retained_ids() {
    let state = make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("hello", &[]),
    ]);
    let cut = make_cut(1, "hello", &[], vec![5, 3], 6);
    let err = validate_branch(
        &state,
        &cut,
        Path::new("state.json"),
        Path::new("display.jsonl"),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("strictly increasing"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_branch_fires_when_retained_id_not_less_than_target() {
    let state = make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("hello", &[]),
    ]);
    let cut = make_cut(1, "hello", &[], vec![6], 6);
    let err = validate_branch(
        &state,
        &cut,
        Path::new("state.json"),
        Path::new("display.jsonl"),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("strictly less"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_branch_fires_when_counter_not_above_max_retained_id() {
    let mut state = make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("hello", &[]),
    ]);
    state.msg_id_counter = 5;
    let cut = make_cut(1, "hello", &[], vec![5], 6);
    let err = validate_branch(
        &state,
        &cut,
        Path::new("state.json"),
        Path::new("display.jsonl"),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("msg_id_counter"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_branch_fires_on_ordinal_out_of_range() {
    let state = make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("hello", &[]),
    ]);
    let cut = make_cut(2, "hello", &[], vec![], 1);
    let err = validate_branch(
        &state,
        &cut,
        Path::new("state.json"),
        Path::new("display.jsonl"),
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("out of range"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// collect_media_refs_from_state
// ---------------------------------------------------------------------------

#[test]
fn collect_media_refs_from_state_collects_and_dedups() {
    let state = make_state(vec![
        LLMMessage::System("system".to_string()),
        user_msg("one", &["a.png"]),
        LLMMessage::User(vec![
            ContentPart::Text("two".to_string()),
            ContentPart::Media(MediaData::new("a.png".to_string(), "image/png".to_string())),
            ContentPart::Media(MediaData::new("b.png".to_string(), "image/png".to_string())),
        ]),
        LLMMessage::ToolResult {
            tool_call_id: "tc1".to_string(),
            content: vec![
                ContentPart::Media(MediaData::new("c.png".to_string(), "image/png".to_string())),
                ContentPart::Text("done".to_string()),
            ],
        },
    ]);
    let refs = collect_media_refs_from_state(&state.llm_msgs);
    assert_eq!(
        refs,
        vec![
            "a.png".to_string(),
            "b.png".to_string(),
            "c.png".to_string()
        ]
    );
}

// ---------------------------------------------------------------------------
// collect_media_refs_from_display
// ---------------------------------------------------------------------------

#[test]
fn collect_media_refs_from_display_skips_none_and_dedups() {
    let envelopes = vec![
        envelope(
            1,
            Message::AssistantMediaOutput {
                media_id: "m1".to_string(),
                end_status: MessageEndStatus::Succeeded,
                media: Some(MediaData::new("x.png".to_string(), "image/png".to_string())),
            },
        ),
        envelope(
            2,
            Message::AssistantMediaOutput {
                media_id: "m2".to_string(),
                end_status: MessageEndStatus::Failed,
                media: None,
            },
        ),
        envelope(
            3,
            Message::AssistantMediaOutput {
                media_id: "m3".to_string(),
                end_status: MessageEndStatus::Succeeded,
                media: Some(MediaData::new("x.png".to_string(), "image/png".to_string())),
            },
        ),
    ];
    let refs = collect_media_refs_from_display(&envelopes);
    assert_eq!(refs, vec!["x.png".to_string()]);
}

// ---------------------------------------------------------------------------
// collect_subagent_ids / collect_tool_call_ids
// ---------------------------------------------------------------------------

#[test]
fn collect_subagent_ids_covers_start_and_continue_and_dedups() {
    let envelopes = vec![
        envelope(
            1,
            Message::SubAgentStart {
                tool_call_id: "t1".to_string(),
                conversation_id: "sub1".to_string(),
                description: "d".to_string(),
            },
        ),
        envelope(
            2,
            Message::SubAgentContinue {
                tool_call_id: "t2".to_string(),
                conversation_id: "sub1".to_string(),
                description: "d".to_string(),
            },
        ),
        envelope(
            3,
            Message::SubAgentStart {
                tool_call_id: "t3".to_string(),
                conversation_id: "sub2".to_string(),
                description: "d".to_string(),
            },
        ),
    ];
    let ids = collect_subagent_ids(&envelopes);
    assert_eq!(ids, vec!["sub1".to_string(), "sub2".to_string()]);
}

#[test]
fn collect_tool_call_ids_covers_both_start_types_and_dedups() {
    let envelopes = vec![
        envelope(
            1,
            Message::AssistantToolCallStart {
                tool_call_index: 0,
                tool_call_id: "call1".to_string(),
                tool_name: "bash".to_string(),
                created_at: 1,
            },
        ),
        envelope(
            2,
            Message::ToolMessageStart {
                tool_call_id: "call1".to_string(),
                created_at: 1,
                tool_name: "bash".to_string(),
                tool_args: "{}".to_string(),
            },
        ),
        envelope(
            3,
            Message::AssistantToolCallStart {
                tool_call_index: 1,
                tool_call_id: "call2".to_string(),
                tool_name: "grep".to_string(),
                created_at: 1,
            },
        ),
    ];
    let ids = collect_tool_call_ids(&envelopes);
    assert_eq!(ids, vec!["call1".to_string(), "call2".to_string()]);
}

// ---------------------------------------------------------------------------
// copy_dir_recursive
// ---------------------------------------------------------------------------

#[test]
fn copy_dir_recursive_copies_tree_skips_tmp_and_symlinks() -> Result<()> {
    let dir = temp_dir();
    let src = dir.join("src");
    let dst = dir.join("dst");
    std::fs::create_dir_all(src.join("sub"))?;
    std::fs::write(src.join("a.txt"), "a")?;
    std::fs::write(src.join("sub").join("b.txt"), "b")?;
    std::fs::write(src.join("sub").join("c.tmp"), "tmp")?;
    std::fs::write(src.join("skip.tmp"), "tmp2")?;
    std::os::unix::fs::symlink("a.txt", src.join("link"))?;

    copy_dir_recursive(&src, &dst)?;

    assert!(dst.join("a.txt").is_file());
    assert!(dst.join("sub").join("b.txt").is_file());
    assert!(!dst.join("sub").join("c.tmp").exists());
    assert!(!dst.join("skip.tmp").exists());
    assert!(!dst.join("link").exists());

    let dir_mode = std::fs::metadata(&dst)?.permissions().mode() & 0o777;
    assert_eq!(dir_mode, 0o700);
    let sub_mode = std::fs::metadata(dst.join("sub"))?.permissions().mode() & 0o777;
    assert_eq!(sub_mode, 0o700);
    let file_mode = std::fs::metadata(dst.join("a.txt"))?.permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600);
    let nested_mode = std::fs::metadata(dst.join("sub").join("b.txt"))?
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(nested_mode, 0o600);
    Ok(())
}

#[test]
fn copy_dir_recursive_missing_src_is_ok() -> Result<()> {
    let dir = temp_dir();
    copy_dir_recursive(&dir.join("nope"), &dir.join("dst"))?;
    assert!(!dir.join("dst").exists());
    Ok(())
}

// ---------------------------------------------------------------------------
// build_branch_content (core files, media handling, path safety)
// ---------------------------------------------------------------------------

fn cut_with_envelopes(envelopes: Vec<BroadcastMessage>) -> DisplayCut {
    DisplayCut {
        retained_lines: vec![],
        retained_envelopes: envelopes,
        retained_ids: vec![],
        max_retained_id: None,
        target_ordinal: 1,
        target: Message::UserMessage {
            created_at: 1,
            content: Arc::new("hello".to_string()),
            media_filenames: vec![],
        },
        target_id: 1,
    }
}

fn empty_source_and_staging(dir: &Path) -> (PathBuf, PathBuf) {
    let source = dir.join("source");
    let staging = dir.join("staging");
    std::fs::create_dir_all(&source).expect("create source dir");
    std::fs::create_dir_all(&staging).expect("create staging dir");
    (source, staging)
}

#[test]
fn build_branch_content_writes_core_files_even_when_empty() -> Result<()> {
    let dir = temp_dir();
    let (source, staging) = empty_source_and_staging(&dir);
    let state = make_state(vec![LLMMessage::System("system".to_string())]);
    let cut = cut_with_envelopes(vec![]);

    build_branch_content(&source, &staging, &state, &cut)?;

    // conversation-state.json parses back with the truncated msgs.
    let saved: ConversationState = serde_json::from_str(&std::fs::read_to_string(
        staging.join("conversation-state.json"),
    )?)?;
    assert_eq!(saved.llm_msgs.len(), 1);
    assert!(matches!(saved.llm_msgs[0], LLMMessage::System(_)));
    // display.jsonl always created, even with an empty retained prefix.
    assert_eq!(
        std::fs::read(staging.join("display.jsonl"))?,
        b"",
        "display.jsonl must exist and be empty"
    );
    // session-meta.json written.
    assert!(staging.join("session-meta.json").is_file());
    Ok(())
}

#[test]
fn build_branch_content_skips_malicious_subagent_and_tool_call_ids() -> Result<()> {
    let dir = temp_dir();
    let (source, staging) = empty_source_and_staging(&dir);

    // A real subagent dir that must be copied, and an intermediate dir so the
    // crafted id resolves outside the source dir via `..`.
    std::fs::create_dir_all(source.join("subagent-real1"))?;
    std::fs::write(
        source
            .join("subagent-real1")
            .join("conversation-state.json"),
        "{}",
    )?;
    std::fs::create_dir_all(source.join("subagent-x"))?;
    std::fs::create_dir_all(source.join("tool-call-x"))?;
    std::fs::create_dir_all(dir.join("escaped"))?;
    std::fs::write(dir.join("escaped").join("marker.txt"), "trap")?;
    std::fs::write(dir.join("escaped.jsonl"), "trap")?;

    let state = make_state(vec![LLMMessage::System("system".to_string())]);
    let cut = cut_with_envelopes(vec![
        envelope(
            1,
            Message::SubAgentStart {
                tool_call_id: "t1".to_string(),
                conversation_id: "real1".to_string(),
                description: "d".to_string(),
            },
        ),
        // Resolves through source/subagent-x to dir/escaped (outside source).
        envelope(
            2,
            Message::SubAgentStart {
                tool_call_id: "t2".to_string(),
                conversation_id: "x/../../escaped".to_string(),
                description: "d".to_string(),
            },
        ),
        // Does not resolve at all.
        envelope(
            3,
            Message::SubAgentStart {
                tool_call_id: "t3".to_string(),
                conversation_id: "no-such-dir".to_string(),
                description: "d".to_string(),
            },
        ),
        envelope(
            4,
            Message::AssistantToolCallStart {
                tool_call_index: 0,
                tool_call_id: "x/../../escaped".to_string(),
                tool_name: "bash".to_string(),
                created_at: 1,
            },
        ),
    ]);

    build_branch_content(&source, &staging, &state, &cut)?;

    // Real subagent copied; nothing escaped into or out of the staging dir.
    assert!(
        staging
            .join("subagent-real1")
            .join("conversation-state.json")
            .is_file()
    );
    assert!(!staging.join("subagent-no-such-dir").exists());
    assert!(!staging.join("subagent-x").exists());
    assert!(!staging.join("escaped").exists());
    assert!(!staging.join("escaped.jsonl").exists());
    assert!(
        dir.join("escaped").join("marker.txt").is_file(),
        "source-side trap must stay untouched"
    );
    assert!(
        dir.join("escaped.jsonl").is_file(),
        "source-side trap must stay untouched"
    );
    // state, meta, display, media/, subagent-real1
    assert_eq!(std::fs::read_dir(&staging)?.count(), 5);
    Ok(())
}

#[test]
fn build_branch_content_copies_only_real_media_files() -> Result<()> {
    let dir = temp_dir();
    let (source, staging) = empty_source_and_staging(&dir);
    std::fs::create_dir_all(source.join("media"))?;
    std::fs::write(source.join("media").join("a.png"), "img")?;
    // A file outside the media dir (traversal target) and a directory named
    // like a media file (dir-copy trap).
    std::fs::write(dir.join("b.png"), "trap")?;
    std::fs::create_dir_all(source.join("media").join("c.png"))?;

    let state = make_state(vec![
        LLMMessage::System("system".to_string()),
        LLMMessage::User(vec![
            ContentPart::Media(MediaData::new("a.png".to_string(), "image/png".to_string())),
            // Empty reference (would previously abort the whole branch).
            ContentPart::Media(MediaData::new("".to_string(), "image/png".to_string())),
            // Traversal reference.
            ContentPart::Media(MediaData::new(
                "../b.png".to_string(),
                "image/png".to_string(),
            )),
            // Directory reference.
            ContentPart::Media(MediaData::new("c.png".to_string(), "image/png".to_string())),
            // Missing reference.
            ContentPart::Media(MediaData::new(
                "missing.png".to_string(),
                "image/png".to_string(),
            )),
        ]),
    ]);
    let cut = cut_with_envelopes(vec![]);

    build_branch_content(&source, &staging, &state, &cut)?;

    assert_eq!(std::fs::read(staging.join("media").join("a.png"))?, b"img");
    let copied: Vec<_> = std::fs::read_dir(staging.join("media"))?
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(copied.len(), 1, "only the real file must be copied");
    assert!(!staging.join("media").join("b.png").exists());
    Ok(())
}

// ---------------------------------------------------------------------------
// commit_branch_staging
// ---------------------------------------------------------------------------

#[test]
fn commit_skips_pre_existing_empty_target_dir() -> Result<()> {
    let dir = temp_dir();
    let base = dir.join("base");
    std::fs::create_dir_all(&base)?;
    // Linux rename would silently replace an EMPTY target dir; the pre-check
    // must treat it as a collision instead.
    std::fs::create_dir_all(base.join("bbbbbbbb"))?;

    let staging = base.join("branch-tmp-1-1");
    std::fs::create_dir_all(&staging)?;
    std::fs::write(staging.join("display.jsonl"), "")?;

    let committed = commit_branch_staging(&base, &staging, "bbbbbbbb".to_string())?;

    assert_ne!(committed, "bbbbbbbb");
    assert!(base.join(&committed).join("display.jsonl").is_file());
    assert!(
        base.join("bbbbbbbb").is_dir(),
        "pre-existing empty dir must survive"
    );
    assert!(!staging.exists());
    Ok(())
}

#[test]
fn commit_retries_on_non_empty_collision() -> Result<()> {
    let dir = temp_dir();
    let base = dir.join("base");
    std::fs::create_dir_all(&base)?;
    std::fs::create_dir_all(base.join("aaaaaaaa"))?;
    std::fs::write(base.join("aaaaaaaa").join("marker"), "x")?;

    let staging = base.join("branch-tmp-1-1");
    std::fs::create_dir_all(&staging)?;
    std::fs::write(staging.join("display.jsonl"), "")?;

    let committed = commit_branch_staging(&base, &staging, "aaaaaaaa".to_string())?;

    assert_ne!(committed, "aaaaaaaa");
    assert!(base.join(&committed).join("display.jsonl").is_file());
    assert!(
        base.join("aaaaaaaa").join("marker").is_file(),
        "existing session must be untouched"
    );
    assert!(!staging.exists());
    Ok(())
}

// ---------------------------------------------------------------------------
// shell_quote
// ---------------------------------------------------------------------------

#[test]
fn shell_quote_wraps_and_escapes_single_quotes() {
    assert_eq!(shell_quote("plain"), "'plain'");
    assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("$x `y`"), "'$x `y`'");
}
