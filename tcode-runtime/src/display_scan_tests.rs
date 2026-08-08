use crate::display_scan::{STALE_CLOSE_MARKERS, TREE_MARKERS, line_mentions_any};

#[test]
fn stale_close_markers_match_envelope_lines() {
    let line = r#"{"id":45,"msg":{"ToolMessageStart":{"tool_call_id":"c","tool_name":"read"}}}"#;
    assert!(line_mentions_any(line, STALE_CLOSE_MARKERS));
}

#[test]
fn stale_close_markers_match_legacy_lines() {
    let line = r#"{"SubAgentStart":{"conversation_id":"s","tool_call_id":"c","description":"d"}}"#;
    assert!(line_mentions_any(line, STALE_CLOSE_MARKERS));
}

#[test]
fn stale_close_markers_skip_chunk_lines() {
    let line = r#"{"id":3,"msg":{"AssistantThinkingChunk":{"content":"Let"}}}"#;
    assert!(!line_mentions_any(line, STALE_CLOSE_MARKERS));
}

#[test]
fn tree_markers_match_tool_and_subagent_events() {
    let start =
        r#"{"id":1,"msg":{"AssistantToolCallStart":{"tool_call_id":"c","tool_name":"read"}}}"#;
    assert!(line_mentions_any(start, TREE_MARKERS));
    let sa = r#"{"id":2,"msg":{"SubAgentInputStart":{"tool_call_index":0,"tool_call_id":"c","tool_name":"subagent"}}}"#;
    assert!(line_mentions_any(sa, TREE_MARKERS));
}

#[test]
fn tree_markers_skip_content_chunks() {
    let thinking = r#"{"id":3,"msg":{"AssistantThinkingChunk":{"content":"Let"}}}"#;
    assert!(!line_mentions_any(thinking, TREE_MARKERS));
    let msg_chunk = r#"{"id":4,"msg":{"AssistantMessageChunk":{"content":"Hi"}}}"#;
    assert!(!line_mentions_any(msg_chunk, TREE_MARKERS));
    let tool_output = r#"{"id":5,"msg":{"ToolOutputChunk":{"tool_call_id":"c","content":"out"}}}"#;
    assert!(!line_mentions_any(tool_output, TREE_MARKERS));
}

#[test]
fn escaped_json_content_does_not_cross_match_marker() {
    // Content that merely *mentions* a variant name is JSON-escaped
    // (`\"SubAgentStart\"` in the raw line), so it can never satisfy the
    // quoted marker — the marker requires a literal `"` before the name.
    let line =
        r#"{"id":6,"msg":{"AssistantThinkingChunk":{"content":"about \"SubAgentStart\" events"}}}"#;
    assert!(!line_mentions_any(line, STALE_CLOSE_MARKERS));
    assert!(!line_mentions_any(line, TREE_MARKERS));
}

#[test]
fn distinct_variant_names_do_not_cross_match() {
    // SubAgentInputStart must not satisfy the "SubAgentStart" marker and vice
    // versa, otherwise the stale-close scan would track the wrong subagents.
    let input_start = r#"{"id":7,"msg":{"SubAgentInputStart":{"tool_call_index":0}}}"#;
    assert!(line_mentions_any(input_start, TREE_MARKERS));
    assert!(!line_mentions_any(input_start, STALE_CLOSE_MARKERS));

    let sa_start = r#"{"id":8,"msg":{"SubAgentStart":{"conversation_id":"s"}}}"#;
    assert!(line_mentions_any(sa_start, STALE_CLOSE_MARKERS));
    assert!(line_mentions_any(sa_start, TREE_MARKERS));
}
