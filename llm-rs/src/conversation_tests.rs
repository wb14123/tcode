#[cfg(test)]
mod tests {
    use crate::conversation::{fill_cancelled_tool_results, ConversationClient, ConversationState};
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

    // ======== Per-tool cancellation ========

    #[test]
    fn cancel_tool_unknown_returns_false() {
        let client = ConversationClient::new_for_test();
        assert!(!client.cancel_tool("nonexistent"));
    }

    #[test]
    fn register_cancel_unregister_workflow() {
        let client = ConversationClient::new_for_test();

        // Register a token
        let token = client.register_tool_token("tc1");
        assert!(!token.is_cancelled());

        // Cancel it
        assert!(client.cancel_tool("tc1"));
        assert!(token.is_cancelled());

        // Unregister it
        client.unregister_tool_token("tc1");

        // Now cancel_tool returns false since it's unregistered
        assert!(!client.cancel_tool("tc1"));
    }

    #[test]
    fn cancel_one_tool_leaves_others_running() {
        let client = ConversationClient::new_for_test();

        let token_a = client.register_tool_token("a");
        let token_b = client.register_tool_token("b");

        // Cancel only tool "a"
        assert!(client.cancel_tool("a"));
        assert!(token_a.is_cancelled());
        assert!(!token_b.is_cancelled());

        // Tool "b" is still cancellable
        assert!(client.cancel_tool("b"));
        assert!(token_b.is_cancelled());
    }

    // ======== Conversation-level cancellation ========

    #[test]
    fn cancel_conversation_cancels_all_tools() {
        let client = ConversationClient::new_for_test();

        let tool_a = client.register_tool_token("a");
        let tool_b = client.register_tool_token("b");
        let tool_c = client.register_tool_token("c");

        assert!(!tool_a.is_cancelled());
        assert!(!tool_b.is_cancelled());
        assert!(!tool_c.is_cancelled());

        // Cancelling the conversation cancels all child tool tokens
        client.cancel();

        assert!(tool_a.is_cancelled());
        assert!(tool_b.is_cancelled());
        assert!(tool_c.is_cancelled());
    }

    #[test]
    fn cancel_tool_does_not_cancel_conversation() {
        let client = ConversationClient::new_for_test();

        let tool_a = client.register_tool_token("a");
        let tool_b = client.register_tool_token("b");

        // Cancel individual tool "a"
        client.cancel_tool("a");
        assert!(tool_a.is_cancelled());

        // Conversation cancel token and other tools are NOT cancelled
        let conv_token = client.current_cancel_token();
        assert!(!conv_token.is_cancelled());
        assert!(!tool_b.is_cancelled());
    }

    #[test]
    fn cancel_cascades_to_children() {
        use std::sync::Arc;

        let parent = ConversationClient::new_for_test();
        let child = Arc::new(ConversationClient::new_for_test());
        let grandchild = Arc::new(ConversationClient::new_for_test());

        // Build parent -> child -> grandchild
        child.register_child("grandchild-1".to_string(), Arc::clone(&grandchild));
        parent.register_child("child-1".to_string(), Arc::clone(&child));

        // Register tool tokens at each level
        let parent_tool = parent.register_tool_token("pt");
        let child_tool = child.register_tool_token("ct");
        let grandchild_tool = grandchild.register_tool_token("gt");

        // Nothing cancelled yet
        assert!(!parent_tool.is_cancelled());
        assert!(!child_tool.is_cancelled());
        assert!(!grandchild_tool.is_cancelled());

        // Cancel parent — should cascade to child and grandchild
        parent.cancel();

        assert!(parent_tool.is_cancelled());
        assert!(child.current_cancel_token().is_cancelled());
        assert!(child_tool.is_cancelled());
        assert!(grandchild.current_cancel_token().is_cancelled());
        assert!(grandchild_tool.is_cancelled());
    }

    #[test]
    fn cancel_and_resume() {
        let client = ConversationClient::new_for_test();

        let tool_before = client.register_tool_token("before");

        // Cancel the conversation
        client.cancel();
        assert!(tool_before.is_cancelled());
        assert!(client.current_cancel_token().is_cancelled());

        // Reset the cancel token (simulating what start() does after cancellation)
        client.reset_cancel_token();

        // New cancel token is fresh
        assert!(!client.current_cancel_token().is_cancelled());

        // New tool tokens created after reset are healthy
        let tool_after = client.register_tool_token("after");
        assert!(!tool_after.is_cancelled());

        // Can still cancel individual tools
        assert!(client.cancel_tool("after"));
        assert!(tool_after.is_cancelled());
    }

    #[test]
    fn cancel_is_idempotent() {
        let client = ConversationClient::new_for_test();
        let tool = client.register_tool_token("t1");

        // Multiple cancels should not panic
        client.cancel();
        client.cancel();
        client.cancel();

        assert!(tool.is_cancelled());
    }
}
