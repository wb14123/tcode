#[cfg(test)]
mod tests {
    use crate::conversation::{fill_cancelled_tool_results, ConversationState};
    use crate::llm::{ChatOptions, LLMMessage, ReasoningEffort, ToolCall};

    fn make_tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
        }
    }

    // ======== ConversationState serde round-trip ========

    #[test]
    fn conversation_state_serde_roundtrip() {
        let state = ConversationState {
            id: "test-conv-1".to_string(),
            model: "claude-opus-4-6".to_string(),
            llm_msgs: vec![
                LLMMessage::System("You are helpful.".to_string()),
                LLMMessage::User("Hello".to_string()),
                LLMMessage::Assistant {
                    content: "Hi there!".to_string(),
                    tool_calls: vec![make_tool_call("tc1", "web_search")],
                    raw: Some(serde_json::json!({"type": "message", "thinking": [1, 2, 3]})),
                },
                LLMMessage::ToolResult {
                    tool_call_id: "tc1".to_string(),
                    content: "Search results...".to_string(),
                },
                LLMMessage::Assistant {
                    content: "Based on the search...".to_string(),
                    tool_calls: vec![],
                    raw: None,
                },
            ],
            chat_options: ChatOptions {
                max_tokens: Some(4096),
                reasoning_effort: Some(ReasoningEffort::Medium),
                reasoning_budget: None,
                exclude_reasoning: false,
            },
            msg_id_counter: 42,
            total_input_tokens: 1000,
            total_output_tokens: 500,
            single_turn: false,
            subagent_depth: 0,
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let deserialized: ConversationState = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, state.id);
        assert_eq!(deserialized.model, state.model);
        assert_eq!(deserialized.llm_msgs.len(), state.llm_msgs.len());
        assert_eq!(deserialized.msg_id_counter, 42);
        assert_eq!(deserialized.total_input_tokens, 1000);
        assert_eq!(deserialized.total_output_tokens, 500);
        assert!(!deserialized.single_turn);
        assert_eq!(deserialized.subagent_depth, 0);

        // Verify chat_options
        assert_eq!(deserialized.chat_options.max_tokens, Some(4096));
        assert_eq!(deserialized.chat_options.reasoning_effort, Some(ReasoningEffort::Medium));
    }

    // ======== LLMMessage serde round-trip ========

    #[test]
    fn llm_message_system_serde() {
        let msg = LLMMessage::System("system prompt".to_string());
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: LLMMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            LLMMessage::System(s) => assert_eq!(s, "system prompt"),
            _ => panic!("Expected System variant"),
        }
    }

    #[test]
    fn llm_message_user_serde() {
        let msg = LLMMessage::User("hello".to_string());
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: LLMMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            LLMMessage::User(s) => assert_eq!(s, "hello"),
            _ => panic!("Expected User variant"),
        }
    }

    #[test]
    fn llm_message_assistant_with_raw_serde() {
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
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: LLMMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            LLMMessage::Assistant { content, tool_calls, raw: r } => {
                assert_eq!(content, "Hello!");
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].id, "tc1");
                assert_eq!(tool_calls[0].name, "search");
                assert_eq!(r, Some(raw));
            }
            _ => panic!("Expected Assistant variant"),
        }
    }

    #[test]
    fn llm_message_tool_result_serde() {
        let msg = LLMMessage::ToolResult {
            tool_call_id: "tc1".to_string(),
            content: "result data".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: LLMMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            LLMMessage::ToolResult { tool_call_id, content } => {
                assert_eq!(tool_call_id, "tc1");
                assert_eq!(content, "result data");
            }
            _ => panic!("Expected ToolResult variant"),
        }
    }

    // ======== ChatOptions serde round-trip ========

    #[test]
    fn chat_options_serde_all_fields() {
        let opts = ChatOptions {
            max_tokens: Some(8192),
            reasoning_effort: Some(ReasoningEffort::High),
            reasoning_budget: None,
            exclude_reasoning: true,
        };
        let json = serde_json::to_string(&opts).unwrap();
        let deserialized: ChatOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_tokens, Some(8192));
        assert_eq!(deserialized.reasoning_effort, Some(ReasoningEffort::High));
        assert!(deserialized.exclude_reasoning);
    }

    #[test]
    fn chat_options_serde_defaults() {
        let opts = ChatOptions::default();
        let json = serde_json::to_string(&opts).unwrap();
        let deserialized: ChatOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_tokens, None);
        assert_eq!(deserialized.reasoning_effort, None);
        assert_eq!(deserialized.reasoning_budget, None);
        assert!(!deserialized.exclude_reasoning);
    }

    // ======== ReasoningEffort serde ========

    #[test]
    fn reasoning_effort_serde_all_variants() {
        for effort in [
            ReasoningEffort::XHigh,
            ReasoningEffort::High,
            ReasoningEffort::Medium,
            ReasoningEffort::Low,
            ReasoningEffort::Minimal,
        ] {
            let json = serde_json::to_string(&effort).unwrap();
            let deserialized: ReasoningEffort = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, effort);
        }
    }

    // ======== ToolCall serde ========

    #[test]
    fn tool_call_serde() {
        let tc = ToolCall {
            id: "call_123".to_string(),
            name: "web_fetch".to_string(),
            arguments: r#"{"url":"https://example.com"}"#.to_string(),
        };
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "call_123");
        assert_eq!(deserialized.name, "web_fetch");
        assert_eq!(deserialized.arguments, r#"{"url":"https://example.com"}"#);
    }

    // ======== fill_cancelled_tool_results ========

    #[test]
    fn fill_cancelled_empty_vec() {
        let mut msgs: Vec<LLMMessage> = vec![];
        fill_cancelled_tool_results(&mut msgs);
        assert!(msgs.is_empty());
    }

    #[test]
    fn fill_cancelled_no_assistant() {
        let mut msgs = vec![
            LLMMessage::System("sys".to_string()),
            LLMMessage::User("hello".to_string()),
        ];
        fill_cancelled_tool_results(&mut msgs);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn fill_cancelled_no_tool_calls_in_last_assistant() {
        let mut msgs = vec![
            LLMMessage::User("hello".to_string()),
            LLMMessage::Assistant {
                content: "hi".to_string(),
                tool_calls: vec![],
                raw: None,
            },
        ];
        fill_cancelled_tool_results(&mut msgs);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn fill_cancelled_all_results_present() {
        let mut msgs = vec![
            LLMMessage::User("hello".to_string()),
            LLMMessage::Assistant {
                content: "".to_string(),
                tool_calls: vec![
                    make_tool_call("a", "tool_a"),
                    make_tool_call("b", "tool_b"),
                ],
                raw: None,
            },
            LLMMessage::ToolResult { tool_call_id: "a".to_string(), content: "result a".to_string() },
            LLMMessage::ToolResult { tool_call_id: "b".to_string(), content: "result b".to_string() },
        ];
        fill_cancelled_tool_results(&mut msgs);
        assert_eq!(msgs.len(), 4); // No change
    }

    #[test]
    fn fill_cancelled_partial_results() {
        let mut msgs = vec![
            LLMMessage::User("hello".to_string()),
            LLMMessage::Assistant {
                content: "".to_string(),
                tool_calls: vec![
                    make_tool_call("a", "tool_a"),
                    make_tool_call("b", "tool_b"),
                    make_tool_call("c", "tool_c"),
                ],
                raw: None,
            },
            LLMMessage::ToolResult { tool_call_id: "a".to_string(), content: "result a".to_string() },
        ];
        fill_cancelled_tool_results(&mut msgs);
        assert_eq!(msgs.len(), 5); // Added 2 cancelled results for b and c
        match &msgs[3] {
            LLMMessage::ToolResult { tool_call_id, content } => {
                assert_eq!(tool_call_id, "b");
                assert!(content.contains("cancelled"));
            }
            _ => panic!("Expected ToolResult"),
        }
        match &msgs[4] {
            LLMMessage::ToolResult { tool_call_id, content } => {
                assert_eq!(tool_call_id, "c");
                assert!(content.contains("cancelled"));
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[test]
    fn fill_cancelled_no_results_at_all() {
        let mut msgs = vec![
            LLMMessage::User("hello".to_string()),
            LLMMessage::Assistant {
                content: "".to_string(),
                tool_calls: vec![
                    make_tool_call("a", "tool_a"),
                    make_tool_call("b", "tool_b"),
                ],
                raw: None,
            },
        ];
        fill_cancelled_tool_results(&mut msgs);
        assert_eq!(msgs.len(), 4); // Added 2 cancelled results
        match &msgs[2] {
            LLMMessage::ToolResult { tool_call_id, .. } => assert_eq!(tool_call_id, "a"),
            _ => panic!("Expected ToolResult"),
        }
        match &msgs[3] {
            LLMMessage::ToolResult { tool_call_id, .. } => assert_eq!(tool_call_id, "b"),
            _ => panic!("Expected ToolResult"),
        }
    }
}
