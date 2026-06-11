#[cfg(test)]
mod tests {
    use crate::llm::{ChatOptions, LLMMessage, ReasoningEffort, ToolCall};
    use crate::media::ContentPart;

    fn make_tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
        }
    }

    // ======== LLMMessage serde round-trip ========

    #[test]
    fn llm_message_system_serde() -> anyhow::Result<()> {
        let msg = LLMMessage::System("system prompt".to_string());
        let json = serde_json::to_string(&msg)?;
        let deserialized: LLMMessage = serde_json::from_str(&json)?;
        match deserialized {
            LLMMessage::System(s) => assert_eq!(s, "system prompt"),
            _ => panic!("Expected System variant"),
        }
        Ok(())
    }

    #[test]
    fn llm_message_user_serde() -> anyhow::Result<()> {
        let msg = LLMMessage::User(vec![ContentPart::Text("hello".to_string())]);
        let json = serde_json::to_string(&msg)?;
        let deserialized: LLMMessage = serde_json::from_str(&json)?;
        match deserialized {
            LLMMessage::User(parts) => {
                assert_eq!(parts.len(), 1);
                match &parts[0] {
                    ContentPart::Text(s) => assert_eq!(s, "hello"),
                    _ => panic!("Expected Text content part"),
                }
            }
            _ => panic!("Expected User variant"),
        }
        Ok(())
    }

    #[test]
    fn llm_message_assistant_with_raw_serde() -> anyhow::Result<()> {
        let raw = serde_json::json!({
            "content": [
                {"type": "thinking", "text": "Let me think..."},
                {"type": "text", "text": "Hello!"}
            ]
        });
        let msg = LLMMessage::Assistant {
            content: "Hello!".to_string(),
            tool_calls: vec![make_tool_call("tc1", "search")],
            raw: Some(raw.clone()),
        };
        let json = serde_json::to_string(&msg)?;
        let deserialized: LLMMessage = serde_json::from_str(&json)?;
        match deserialized {
            LLMMessage::Assistant {
                content,
                tool_calls,
                raw: r,
            } => {
                assert_eq!(content, "Hello!");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "tc1");
                assert_eq!(tool_calls[0].name, "search");
                assert_eq!(r, Some(raw));
            }
            _ => panic!("Expected Assistant variant"),
        }
        Ok(())
    }

    #[test]
    fn llm_message_tool_result_serde() -> anyhow::Result<()> {
        use crate::media::ContentPart;

        let msg = LLMMessage::ToolResult {
            tool_call_id: "tc1".to_string(),
            content: vec![ContentPart::Text("result data".to_string())],
        };
        let json = serde_json::to_string(&msg)?;
        let deserialized: LLMMessage = serde_json::from_str(&json)?;
        match deserialized {
            LLMMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "tc1");
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentPart::Text(t) => assert_eq!(t, "result data"),
                    _ => panic!("Expected Text content"),
                }
            }
            _ => panic!("Expected ToolResult variant"),
        }
        Ok(())
    }

    // ======== ChatOptions serde round-trip ========

    #[test]
    fn chat_options_serde_all_fields() -> anyhow::Result<()> {
        let opts = ChatOptions {
            max_tokens: Some(8192),
            reasoning_effort: Some(ReasoningEffort::High),
            reasoning_budget: None,
            exclude_reasoning: true,
        };
        let json = serde_json::to_string(&opts)?;
        let deserialized: ChatOptions = serde_json::from_str(&json)?;
        assert_eq!(deserialized.max_tokens, Some(8192));
        assert_eq!(deserialized.reasoning_effort, Some(ReasoningEffort::High));
        assert!(deserialized.exclude_reasoning);
        Ok(())
    }

    #[test]
    fn chat_options_serde_defaults() -> anyhow::Result<()> {
        let opts = ChatOptions::default();
        let json = serde_json::to_string(&opts)?;
        let deserialized: ChatOptions = serde_json::from_str(&json)?;
        assert_eq!(deserialized.max_tokens, None);
        assert_eq!(deserialized.reasoning_effort, None);
        assert_eq!(deserialized.reasoning_budget, None);
        assert!(!deserialized.exclude_reasoning);
        Ok(())
    }

    // ======== ReasoningEffort serde ========

    #[test]
    fn reasoning_effort_serde_all_variants() -> anyhow::Result<()> {
        for effort in [
            ReasoningEffort::Max,
            ReasoningEffort::XHigh,
            ReasoningEffort::High,
            ReasoningEffort::Medium,
            ReasoningEffort::Low,
            ReasoningEffort::Minimal,
        ] {
            let json = serde_json::to_string(&effort)?;
            let deserialized: ReasoningEffort = serde_json::from_str(&json)?;
            assert_eq!(deserialized, effort);
        }
        Ok(())
    }

    #[test]
    fn reasoning_effort_as_str() {
        assert_eq!(ReasoningEffort::Max.as_str(), "max");
        assert_eq!(ReasoningEffort::XHigh.as_str(), "xhigh");
        assert_eq!(ReasoningEffort::High.as_str(), "high");
        assert_eq!(ReasoningEffort::Medium.as_str(), "medium");
        assert_eq!(ReasoningEffort::Low.as_str(), "low");
        assert_eq!(ReasoningEffort::Minimal.as_str(), "minimal");
    }

    #[test]
    fn reasoning_effort_as_claude_budget() {
        assert_eq!(ReasoningEffort::Max.as_claude_budget(), 32000);
        assert_eq!(ReasoningEffort::XHigh.as_claude_budget(), 31999);
        assert_eq!(ReasoningEffort::High.as_claude_budget(), 24000);
        assert_eq!(ReasoningEffort::Medium.as_claude_budget(), 16000);
        assert_eq!(ReasoningEffort::Low.as_claude_budget(), 8000);
        assert_eq!(ReasoningEffort::Minimal.as_claude_budget(), 4000);
    }

    #[test]
    fn is_manual_only_model_detects_old_models() {
        use crate::llm::is_manual_only_model;
        // Direct API names
        assert!(is_manual_only_model("claude-opus-4-5"));
        assert!(is_manual_only_model("claude-sonnet-4-5"));
        assert!(is_manual_only_model("claude-haiku-4-5"));
        // Bedrock ARN-format IDs
        assert!(is_manual_only_model("us.anthropic.claude-opus-4-5-v1"));
        assert!(is_manual_only_model("us.anthropic.claude-sonnet-4-5-v1"));
        assert!(is_manual_only_model("us.anthropic.claude-haiku-4-5-v1"));
    }

    #[test]
    fn is_manual_only_model_rejects_new_models() {
        use crate::llm::is_manual_only_model;
        assert!(!is_manual_only_model("claude-opus-4-6"));
        assert!(!is_manual_only_model("claude-sonnet-4-6"));
        assert!(!is_manual_only_model("claude-fable-5"));
        assert!(!is_manual_only_model("claude-mythos-5"));
        assert!(!is_manual_only_model("claude-opus-4-8"));
        assert!(!is_manual_only_model("claude-opus-4-7"));
        assert!(!is_manual_only_model(""));
        assert!(!is_manual_only_model("gpt-5"));
    }

    // ======== ToolCall serde ========

    #[test]
    fn tool_call_serde() -> anyhow::Result<()> {
        let tc = ToolCall {
            id: "call_123".to_string(),
            name: "web_fetch".to_string(),
            arguments: r#"{"url":"https://example.com"}"#.to_string(),
        };
        let json = serde_json::to_string(&tc)?;
        let deserialized: ToolCall = serde_json::from_str(&json)?;
        assert_eq!(deserialized.id, "call_123");
        assert_eq!(deserialized.name, "web_fetch");
        assert_eq!(deserialized.arguments, r#"{"url":"https://example.com"}"#);
        Ok(())
    }
}
