#[cfg(test)]
mod tests {
    use crate::llm::{ChatOptions, LLMMessage, ReasoningEffort, ToolCall};

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
        let msg = LLMMessage::User("hello".to_string());
        let json = serde_json::to_string(&msg)?;
        let deserialized: LLMMessage = serde_json::from_str(&json)?;
        match deserialized {
            LLMMessage::User(s) => assert_eq!(s, "hello"),
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
        let msg = LLMMessage::ToolResult {
            tool_call_id: "tc1".to_string(),
            content: "result data".to_string(),
        };
        let json = serde_json::to_string(&msg)?;
        let deserialized: LLMMessage = serde_json::from_str(&json)?;
        match deserialized {
            LLMMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "tc1");
                assert_eq!(content, "result data");
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
