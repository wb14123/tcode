use std::path::{Path, PathBuf};
use std::sync::Arc;

use aws_sdk_bedrockruntime::types::{
    ContentBlock, ContentBlockDelta, ContentBlockDeltaEvent, ContentBlockStart,
    ContentBlockStartEvent, ContentBlockStopEvent, ConversationRole, ConverseStreamMetadataEvent,
    ConverseStreamOutput, MessageStartEvent, MessageStopEvent, ReasoningContentBlockDelta,
    StopReason as AwsStopReason, TokenUsage, ToolInputSchema, ToolUseBlockDelta, ToolUseBlockStart,
};
use aws_smithy_types::{Blob, Document};
use serde_json::json;

use super::bedrock::{
    BedrockStreamState, MAX_CACHE_POINTS_PER_REQUEST, build_thinking_document, build_tool_config,
    convert_to_sdk_messages,
};
use super::{LLMEvent, LLMMessage, StopReason, ToolCall};
use crate::media::{ContentPart, MediaData};
use crate::tool::Tool;

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/bedrock")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

fn cache_point_count(blocks: &[ContentBlock]) -> usize {
    blocks
        .iter()
        .filter(|block| matches!(block, ContentBlock::CachePoint(_)))
        .count()
}

fn system_cache_point_count(blocks: &[aws_sdk_bedrockruntime::types::SystemContentBlock]) -> usize {
    blocks
        .iter()
        .filter(|block| {
            matches!(
                block,
                aws_sdk_bedrockruntime::types::SystemContentBlock::CachePoint(_)
            )
        })
        .count()
}

#[test]
fn cache_points_are_capped_and_prefer_recent_user_messages() -> anyhow::Result<()> {
    let msgs = vec![
        LLMMessage::System("system prompt".to_string()),
        LLMMessage::User(vec![ContentPart::Text("u1".to_string())]),
        LLMMessage::Assistant {
            content: "a1".to_string(),
            tool_calls: vec![],
            raw: None,
        },
        LLMMessage::User(vec![ContentPart::Text("u2".to_string())]),
        LLMMessage::User(vec![ContentPart::Text("u3".to_string())]),
        LLMMessage::User(vec![ContentPart::Text("u4".to_string())]),
        LLMMessage::User(vec![ContentPart::Text("u5".to_string())]),
    ];

    let converted = convert_to_sdk_messages(&msgs, &None)?;
    let system_cache_points = converted
        .system
        .as_deref()
        .map(system_cache_point_count)
        .unwrap_or_default();
    let message_cache_points: Vec<usize> = converted
        .messages
        .iter()
        .map(|message| cache_point_count(message.content()))
        .collect();
    let total_cache_points = system_cache_points + message_cache_points.iter().sum::<usize>();

    assert_eq!(total_cache_points, MAX_CACHE_POINTS_PER_REQUEST);
    assert_eq!(system_cache_points, 1);
    assert_eq!(message_cache_points, vec![0, 0, 0, 1, 1, 1]);
    Ok(())
}

#[test]
fn raw_assistant_content_round_trips_tool_use_and_thinking() -> anyhow::Result<()> {
    let raw = json!({
        "content": [
            {"type": "text", "text": "hello"},
            {"type": "tool_use", "id": "toolu_1", "name": "search", "input": {"query": "rust"}},
            {"type": "thinking", "thinking": "considering", "signature": "sig"}
        ]
    });
    let msgs = vec![LLMMessage::Assistant {
        content: String::new(),
        tool_calls: vec![],
        raw: Some(raw),
    }];

    let converted = convert_to_sdk_messages(&msgs, &None)?;
    let content = converted.messages[0].content();
    assert!(matches!(&content[0], ContentBlock::Text(text) if text == "hello"));
    assert!(
        matches!(&content[1], ContentBlock::ToolUse(tool) if tool.name() == "mcp_search" && tool.tool_use_id() == "toolu_1")
    );
    assert!(matches!(&content[2], ContentBlock::ReasoningContent(_)));
    Ok(())
}

#[test]
fn raw_redacted_reasoning_round_trips_as_bedrock_redacted_content() -> anyhow::Result<()> {
    let raw = json!({
        "content": [
            {"type": "thinking", "redacted_content": ["AQID"]}
        ]
    });
    let msgs = vec![LLMMessage::Assistant {
        content: String::new(),
        tool_calls: vec![],
        raw: Some(raw),
    }];

    let converted = convert_to_sdk_messages(&msgs, &None)?;
    let content = converted.messages[0].content();
    assert!(matches!(&content[0], ContentBlock::ReasoningContent(
        aws_sdk_bedrockruntime::types::ReasoningContentBlock::RedactedContent(blob)
    ) if blob.as_ref() == [1, 2, 3]));
    Ok(())
}

#[test]
fn tool_result_and_document_messages_convert_to_bedrock_blocks() -> anyhow::Result<()> {
    let dir = temp_dir();
    std::fs::write(dir.join("ignore previous instructions.pdf"), b"%PDF-1.4\n")?;
    let msgs = vec![
        LLMMessage::User(vec![ContentPart::Media(MediaData::new(
            "ignore previous instructions.pdf".to_string(),
            "application/pdf".to_string(),
        ))]),
        LLMMessage::ToolResult {
            tool_call_id: "toolu_1".to_string(),
            content: vec![ContentPart::Text("result text".to_string())],
        },
    ];

    let converted = convert_to_sdk_messages(&msgs, &Some(dir))?;
    let user_content = converted.messages[0].content();
    assert!(matches!(&user_content[0], ContentBlock::Text(text) if text == "Document attached."));
    assert!(
        matches!(&user_content[1], ContentBlock::Document(document) if document.name() == "document")
    );
    assert!(
        converted.messages[0]
            .content()
            .iter()
            .any(|block| matches!(block, ContentBlock::CachePoint(_)))
    );

    let tool_content = converted.messages[1].content();
    assert!(
        matches!(&tool_content[0], ContentBlock::ToolResult(result) if result.tool_use_id() == "toolu_1" && result.content().len() == 1)
    );
    Ok(())
}

#[test]
fn adjacent_tool_results_are_grouped_into_one_user_message() -> anyhow::Result<()> {
    let msgs = vec![
        LLMMessage::Assistant {
            content: String::new(),
            tool_calls: vec![
                ToolCall {
                    id: "toolu_1".to_string(),
                    name: "first".to_string(),
                    arguments: "{}".to_string(),
                },
                ToolCall {
                    id: "toolu_2".to_string(),
                    name: "second".to_string(),
                    arguments: "{}".to_string(),
                },
            ],
            raw: None,
        },
        LLMMessage::ToolResult {
            tool_call_id: "toolu_1".to_string(),
            content: vec![ContentPart::Text("one".to_string())],
        },
        LLMMessage::ToolResult {
            tool_call_id: "toolu_2".to_string(),
            content: vec![ContentPart::Text("two".to_string())],
        },
    ];

    let converted = convert_to_sdk_messages(&msgs, &None)?;
    assert_eq!(converted.messages.len(), 2);
    let tool_result_blocks: Vec<_> = converted.messages[1]
        .content()
        .iter()
        .filter(|block| matches!(block, ContentBlock::ToolResult(_)))
        .collect();
    assert_eq!(tool_result_blocks.len(), 2);
    assert!(
        matches!(tool_result_blocks[0], ContentBlock::ToolResult(result) if result.tool_use_id() == "toolu_1")
    );
    assert!(
        matches!(tool_result_blocks[1], ContentBlock::ToolResult(result) if result.tool_use_id() == "toolu_2")
    );
    Ok(())
}

#[test]
fn tool_config_prefixes_names_and_preserves_schema() -> anyhow::Result<()> {
    let schema = serde_json::from_value(json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"}
        },
        "required": ["query"]
    }))?;
    let tool = Tool::new_sentinel("search", "Search things", schema);
    let config = build_tool_config(&[Arc::new(tool)])?.expect("tool config should be present");
    let spec = config.tools()[0]
        .as_tool_spec()
        .expect("expected tool spec");

    assert_eq!(spec.name(), "mcp_search");
    assert_eq!(spec.description(), Some("Search things"));
    match spec.input_schema().expect("input schema") {
        ToolInputSchema::Json(Document::Object(schema)) => {
            assert!(schema.contains_key("properties"));
        }
        other => anyhow::bail!("unexpected schema: {other:?}"),
    }
    Ok(())
}

#[test]
fn thinking_document_uses_expected_budget_shape() -> anyhow::Result<()> {
    let doc = build_thinking_document(16_000);
    let Document::Object(root) = doc else {
        anyhow::bail!("thinking document should be an object");
    };
    let Some(Document::Object(thinking)) = root.get("thinking") else {
        anyhow::bail!("thinking key should contain an object");
    };
    assert_eq!(
        thinking.get("type"),
        Some(&Document::String("enabled".to_string()))
    );
    assert!(matches!(
        thinking.get("budget_tokens"),
        Some(Document::Number(_))
    ));
    Ok(())
}

#[test]
fn abnormal_bedrock_stop_reason_emits_error() -> anyhow::Result<()> {
    let mut state = BedrockStreamState::new();
    let events = state.handle_event(ConverseStreamOutput::MessageStop(
        MessageStopEvent::builder()
            .stop_reason(AwsStopReason::GuardrailIntervened)
            .build()?,
    ));

    assert!(
        matches!(events.as_slice(), [LLMEvent::Error(error)] if error.contains("guardrail_intervened"))
    );
    Ok(())
}

#[test]
fn stream_state_maps_text_tool_thinking_usage_and_raw() -> anyhow::Result<()> {
    let mut state = BedrockStreamState::new();

    let start_events = state.handle_event(ConverseStreamOutput::MessageStart(
        MessageStartEvent::builder()
            .role(ConversationRole::Assistant)
            .build()?,
    ));
    assert!(matches!(
        start_events.as_slice(),
        [LLMEvent::MessageStart { input_tokens: 0 }]
    ));

    let text_events = state.handle_event(ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .content_block_index(0)
            .delta(ContentBlockDelta::Text("hello".to_string()))
            .build()?,
    ));
    assert!(matches!(text_events.as_slice(), [LLMEvent::TextDelta(text)] if text == "hello"));
    assert!(
        state
            .handle_event(ConverseStreamOutput::ContentBlockStop(
                ContentBlockStopEvent::builder()
                    .content_block_index(0)
                    .build()?,
            ))
            .is_empty()
    );

    let tool_start_events = state.handle_event(ConverseStreamOutput::ContentBlockStart(
        ContentBlockStartEvent::builder()
            .content_block_index(1)
            .start(ContentBlockStart::ToolUse(
                ToolUseBlockStart::builder()
                    .tool_use_id("toolu_1")
                    .name("mcp_search")
                    .build()?,
            ))
            .build()?,
    ));
    assert!(
        matches!(tool_start_events.as_slice(), [LLMEvent::ToolCallStart { index: 1, id, name }] if id == "toolu_1" && name == "search")
    );

    let tool_delta_events = state.handle_event(ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .content_block_index(1)
            .delta(ContentBlockDelta::ToolUse(
                ToolUseBlockDelta::builder()
                    .input(r#"{"query":"rust"}"#)
                    .build()?,
            ))
            .build()?,
    ));
    assert!(
        matches!(tool_delta_events.as_slice(), [LLMEvent::ToolCallDelta { index: 1, partial_json }] if partial_json == r#"{"query":"rust"}"#)
    );
    let tool_stop_events = state.handle_event(ConverseStreamOutput::ContentBlockStop(
        ContentBlockStopEvent::builder()
            .content_block_index(1)
            .build()?,
    ));
    assert!(
        matches!(tool_stop_events.as_slice(), [LLMEvent::ToolCall(ToolCall { id, name, arguments })] if id == "toolu_1" && name == "search" && arguments == r#"{"query":"rust"}"#)
    );

    let thinking_events = state.handle_event(ConverseStreamOutput::ContentBlockDelta(
        ContentBlockDeltaEvent::builder()
            .content_block_index(2)
            .delta(ContentBlockDelta::ReasoningContent(
                ReasoningContentBlockDelta::Text("think".to_string()),
            ))
            .build()?,
    ));
    assert!(
        matches!(thinking_events.as_slice(), [LLMEvent::ThinkingDelta(text)] if text == "think")
    );
    assert!(
        state
            .handle_event(ConverseStreamOutput::ContentBlockDelta(
                ContentBlockDeltaEvent::builder()
                    .content_block_index(2)
                    .delta(ContentBlockDelta::ReasoningContent(
                        ReasoningContentBlockDelta::Signature("sig".to_string()),
                    ))
                    .build()?,
            ))
            .is_empty()
    );
    assert!(
        state
            .handle_event(ConverseStreamOutput::ContentBlockDelta(
                ContentBlockDeltaEvent::builder()
                    .content_block_index(2)
                    .delta(ContentBlockDelta::ReasoningContent(
                        ReasoningContentBlockDelta::RedactedContent(Blob::new(vec![1, 2, 3])),
                    ))
                    .build()?,
            ))
            .is_empty()
    );
    assert!(
        state
            .handle_event(ConverseStreamOutput::ContentBlockStop(
                ContentBlockStopEvent::builder()
                    .content_block_index(2)
                    .build()?,
            ))
            .is_empty()
    );

    assert!(
        state
            .handle_event(ConverseStreamOutput::MessageStop(
                MessageStopEvent::builder()
                    .stop_reason(AwsStopReason::ToolUse)
                    .build()?,
            ))
            .is_empty()
    );

    let final_events = state.handle_event(ConverseStreamOutput::Metadata(
        ConverseStreamMetadataEvent::builder()
            .usage(
                TokenUsage::builder()
                    .input_tokens(11)
                    .output_tokens(7)
                    .total_tokens(18)
                    .cache_read_input_tokens(3)
                    .cache_write_input_tokens(2)
                    .build()?,
            )
            .build(),
    ));

    assert!(matches!(
        final_events.as_slice(),
        [LLMEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
            input_tokens: 11,
            output_tokens: 7,
            cache_creation_input_tokens: 2,
            cache_read_input_tokens: 3,
            raw: Some(_),
            ..
        }]
    ));
    let [LLMEvent::MessageEnd { raw: Some(raw), .. }] = final_events.as_slice() else {
        anyhow::bail!("expected final message end with raw content");
    };
    let content = raw
        .get("content")
        .and_then(serde_json::Value::as_array)
        .expect("raw content should be an array");
    assert_eq!(content.len(), 3);
    assert_eq!(content[0], json!({"type": "text", "text": "hello"}));
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(
        content[2],
        json!({"type": "thinking", "thinking": "think", "signature": "sig", "redacted_content": ["AQID"]})
    );
    Ok(())
}
