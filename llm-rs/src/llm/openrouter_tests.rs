use std::collections::HashMap;

use super::LLMMessage;
use super::openrouter::{Usage, build_raw_assistant_message, convert_messages, extract_usage};

#[test]
fn build_raw_assistant_message_preserves_reasoning_content_field() -> anyhow::Result<()> {
    let tool_calls = HashMap::new();

    let raw = build_raw_assistant_message("answer", &tool_calls, &[], "thinking text");

    assert_eq!(raw["role"], "assistant");
    assert_eq!(raw["content"], "answer");
    assert_eq!(raw["reasoning_content"], "thinking text");
    assert!(raw.get("reasoning_details").is_none());

    Ok(())
}

#[test]
fn build_raw_assistant_message_keeps_reasoning_details_separate() -> anyhow::Result<()> {
    let tool_calls = HashMap::new();
    let details = vec![serde_json::json!({
        "type": "reasoning.encrypted",
        "data": "opaque"
    })];

    let raw = build_raw_assistant_message("answer", &tool_calls, &details, "thinking text");

    assert_eq!(raw["reasoning_details"], serde_json::json!(details));
    assert_eq!(raw["reasoning_content"], "thinking text");

    Ok(())
}

#[test]
fn convert_messages_replays_assistant_raw_without_dropping_reasoning_content() -> anyhow::Result<()>
{
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "answer",
        "reasoning_content": "thinking text",
        "provider_extra": { "kept": true }
    });
    let msgs = vec![LLMMessage::Assistant {
        content: "fallback content".to_string(),
        tool_calls: Vec::new(),
        raw: Some(raw.clone()),
    }];

    let serialized = serde_json::to_value(convert_messages(&msgs))?;

    assert_eq!(serialized, serde_json::json!([raw]));

    Ok(())
}

#[test]
fn convert_messages_forces_assistant_role_for_raw_message() -> anyhow::Result<()> {
    let raw = serde_json::json!({
        "role": "system",
        "content": "answer",
        "reasoning_content": "thinking text"
    });
    let msgs = vec![LLMMessage::Assistant {
        content: "fallback content".to_string(),
        tool_calls: Vec::new(),
        raw: Some(raw),
    }];

    let serialized = serde_json::to_value(convert_messages(&msgs))?;

    assert_eq!(serialized[0]["role"], "assistant");
    assert_eq!(serialized[0]["reasoning_content"], "thinking text");

    Ok(())
}

#[test]
fn extract_usage_splits_openrouter_prompt_cache_tokens() -> anyhow::Result<()> {
    let usage: Usage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 10339,
        "completion_tokens": 60,
        "prompt_tokens_details": {
            "cached_tokens": 10318,
            "cache_write_tokens": 20
        },
        "output_tokens_details": {
            "reasoning_tokens": 7
        }
    }))?;

    let (input_tokens, output_tokens, reasoning_tokens, cache_creation_tokens, cache_read_tokens) =
        extract_usage(&usage);

    assert_eq!(input_tokens, 1);
    assert_eq!(output_tokens, 60);
    assert_eq!(reasoning_tokens, 7);
    assert_eq!(cache_creation_tokens, 20);
    assert_eq!(cache_read_tokens, 10318);

    Ok(())
}

#[test]
fn extract_usage_defaults_cache_tokens_when_details_missing() -> anyhow::Result<()> {
    let usage: Usage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 123,
        "completion_tokens": 45
    }))?;

    let (input_tokens, output_tokens, reasoning_tokens, cache_creation_tokens, cache_read_tokens) =
        extract_usage(&usage);

    assert_eq!(input_tokens, 123);
    assert_eq!(output_tokens, 45);
    assert_eq!(reasoning_tokens, 0);
    assert_eq!(cache_creation_tokens, 0);
    assert_eq!(cache_read_tokens, 0);

    Ok(())
}

#[test]
fn extract_usage_clamps_input_tokens_if_cache_details_exceed_prompt_tokens() -> anyhow::Result<()> {
    let usage: Usage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 10,
        "completion_tokens": 2,
        "prompt_tokens_details": {
            "cached_tokens": 9,
            "cache_write_tokens": 9
        }
    }))?;

    let (input_tokens, output_tokens, reasoning_tokens, cache_creation_tokens, cache_read_tokens) =
        extract_usage(&usage);

    assert_eq!(input_tokens, 0);
    assert_eq!(output_tokens, 2);
    assert_eq!(reasoning_tokens, 0);
    assert_eq!(cache_creation_tokens, 9);
    assert_eq!(cache_read_tokens, 9);

    Ok(())
}
