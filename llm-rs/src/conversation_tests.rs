#[cfg(test)]
mod tests {
    use crate::conversation::{
        ConversationClient, ConversationManager, ConversationState, ConversationSummary,
        SystemPromptContext, create_subagent_tool, fill_cancelled_tool_results,
    };
    use crate::llm::{
        ChatOptions, LLMEvent, LLMMessage, ModelInfo, ReasoningEffort, StopReason, ToolCall,
    };
    use crate::media::ContentPart;
    use crate::tool::Tool;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
    use tokio_stream::{Stream, StreamExt};

    fn make_tool_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: "{}".to_string(),
        }
    }

    /// Per-test temp dir under the workspace target dir; removed on drop
    /// (cleanup runs on success and on panic).
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(module: &str) -> Self {
            let root =
                Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../target/test-tmp/{module}"));
            std::fs::create_dir_all(&root).expect("failed to create test root");
            let dir = root.join(uuid::Uuid::new_v4().to_string());
            // Cleanup before: remove any stale leftover at this exact path.
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("failed to create test dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // ======== Session metadata and mode ========

    // ======== Conversation summary ========

    #[test]
    fn conversation_summary_serde_roundtrip() -> anyhow::Result<()> {
        let summary = ConversationSummary {
            description: Some("hello".to_string()),
            created_at: Some(123),
            last_active_at: Some(456),
        };

        let json = serde_json::to_string_pretty(&summary)?;
        let deserialized: ConversationSummary = serde_json::from_str(&json)?;

        assert_eq!(deserialized, summary);
        Ok(())
    }

    #[test]
    fn conversation_state_summary_uses_first_user_message() {
        let state = ConversationState {
            id: "test-conv-1".to_string(),
            model: "claude-opus-4-6".to_string(),
            llm_msgs: vec![
                LLMMessage::System("You are helpful.".to_string()),
                LLMMessage::User(vec![ContentPart::Text(
                    "Hello from an old state".to_string(),
                )]),
            ],
            chat_options: ChatOptions::default(),
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
        };

        let summary = state.summary();

        assert_eq!(
            summary.description.as_deref(),
            Some("Hello from an old state")
        );
        assert_eq!(summary.created_at, None);
        assert_eq!(summary.last_active_at, None);
    }

    // ======== System prompt builder ========

    #[test]
    fn conversation_manager_uses_provided_system_prompt_builder() -> anyhow::Result<()> {
        let dir = TestDir::new("conversation");
        let manager = ConversationManager::new_with_system_prompt_builder(
            dir.path().join("permissions.json"),
            None,
            std::sync::Arc::new(|context: SystemPromptContext| {
                format!("custom prompt depth {}", context.subagent_depth)
            }),
        );

        assert_eq!(manager.build_system_prompt(2), "custom prompt depth 2");
        Ok(())
    }

    #[test]
    fn subagent_tool_description_is_mode_neutral() {
        let tool = create_subagent_tool(&[ModelInfo {
            id: "model-a".to_string(),
            description: "general model".to_string(),
        }]);

        assert!(tool.description.contains("self-contained task"));
        assert!(tool.description.contains("model-a"));
        for local_phrase in [
            "implementation subtasks",
            "debugging",
            "verification",
            "fix-and-verify",
            "file paths",
            "function names",
            "code change",
        ] {
            assert!(
                !tool.description.contains(local_phrase),
                "subagent description should not include local/code-specific phrase: {local_phrase}"
            );
        }
    }

    // ======== ConversationState serde round-trip ========

    #[test]
    fn conversation_state_serde_roundtrip() -> anyhow::Result<()> {
        let state = ConversationState {
            id: "test-conv-1".to_string(),
            model: "claude-opus-4-6".to_string(),
            llm_msgs: vec![
                LLMMessage::System("You are helpful.".to_string()),
                LLMMessage::User(vec![ContentPart::Text("Hello".to_string())]),
                LLMMessage::Assistant {
                    content: "Hi there!".to_string(),
                    tool_calls: vec![make_tool_call("tc1", "web_search")],
                    raw: Some(serde_json::json!({"type": "message", "thinking": [1, 2, 3]})),
                },
                LLMMessage::ToolResult {
                    tool_call_id: "tc1".to_string(),
                    content: vec![ContentPart::Text("Search results...".to_string())],
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
                connect_timeout_secs: None,
                request_timeout_secs: None,
                media_generation_timeout_secs: None,
                max_retries: None,
            },
            total_input_tokens: 1000,
            total_output_tokens: 500,
            total_cache_creation_tokens: 0,
            total_cache_read_tokens: 0,
            single_turn: false,
            subagent_depth: 0,
            aggregate_input_tokens: 0,
            aggregate_output_tokens: 0,
            aggregate_cache_creation_tokens: 0,
            aggregate_cache_read_tokens: 0,
        };

        let json = serde_json::to_string_pretty(&state)?;
        let deserialized: ConversationState = serde_json::from_str(&json)?;

        assert_eq!(deserialized.id, state.id);
        assert_eq!(deserialized.model, state.model);
        assert_eq!(deserialized.llm_msgs.len(), state.llm_msgs.len());
        assert_eq!(deserialized.total_input_tokens, 1000);
        assert_eq!(deserialized.total_output_tokens, 500);
        assert!(!deserialized.single_turn);
        assert_eq!(deserialized.subagent_depth, 0);

        // Verify chat_options
        assert_eq!(deserialized.chat_options.max_tokens, Some(4096));
        assert_eq!(
            deserialized.chat_options.reasoning_effort,
            Some(ReasoningEffort::Medium)
        );
        Ok(())
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
            LLMMessage::User(vec![ContentPart::Text("hello".to_string())]),
        ];
        fill_cancelled_tool_results(&mut msgs);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn fill_cancelled_no_tool_calls_in_last_assistant() {
        let mut msgs = vec![
            LLMMessage::User(vec![ContentPart::Text("hello".to_string())]),
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
            LLMMessage::User(vec![ContentPart::Text("hello".to_string())]),
            LLMMessage::Assistant {
                content: "".to_string(),
                tool_calls: vec![make_tool_call("a", "tool_a"), make_tool_call("b", "tool_b")],
                raw: None,
            },
            LLMMessage::ToolResult {
                tool_call_id: "a".to_string(),
                content: vec![ContentPart::Text("result a".to_string())],
            },
            LLMMessage::ToolResult {
                tool_call_id: "b".to_string(),
                content: vec![ContentPart::Text("result b".to_string())],
            },
        ];
        fill_cancelled_tool_results(&mut msgs);
        assert_eq!(msgs.len(), 4); // No change
    }

    #[test]
    fn fill_cancelled_partial_results() {
        let mut msgs = vec![
            LLMMessage::User(vec![ContentPart::Text("hello".to_string())]),
            LLMMessage::Assistant {
                content: "".to_string(),
                tool_calls: vec![
                    make_tool_call("a", "tool_a"),
                    make_tool_call("b", "tool_b"),
                    make_tool_call("c", "tool_c"),
                ],
                raw: None,
            },
            LLMMessage::ToolResult {
                tool_call_id: "a".to_string(),
                content: vec![ContentPart::Text("result a".to_string())],
            },
        ];
        fill_cancelled_tool_results(&mut msgs);
        assert_eq!(msgs.len(), 5); // Added 2 cancelled results for b and c
        match &msgs[3] {
            LLMMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "b");
                let has_cancelled = content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t.contains("cancelled")));
                assert!(has_cancelled);
            }
            _ => panic!("Expected ToolResult"),
        }
        match &msgs[4] {
            LLMMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                assert_eq!(tool_call_id, "c");
                let has_cancelled = content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t.contains("cancelled")));
                assert!(has_cancelled);
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[test]
    fn fill_cancelled_no_results_at_all() {
        let mut msgs = vec![
            LLMMessage::User(vec![ContentPart::Text("hello".to_string())]),
            LLMMessage::Assistant {
                content: "".to_string(),
                tool_calls: vec![make_tool_call("a", "tool_a"), make_tool_call("b", "tool_b")],
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

    // ======== cancel_silent ========

    #[test]
    fn cancel_silent_cancels_tools_without_system_message() {
        let client = ConversationClient::new_for_test();

        let tool_a = client.register_tool_token("a");
        let tool_b = client.register_tool_token("b");

        assert!(!tool_a.is_cancelled());
        assert!(!tool_b.is_cancelled());

        // cancel_silent cancels all tools
        client.cancel_silent();

        assert!(tool_a.is_cancelled());
        assert!(tool_b.is_cancelled());
        assert!(client.current_cancel_token().is_cancelled());

        // No system message was broadcast (no subscribers would see one,
        // but importantly it doesn't panic on the broadcast channel)
    }

    #[test]
    fn cancel_silent_cascades_to_children() {
        use std::sync::Arc;

        let parent = ConversationClient::new_for_test();
        let child = Arc::new(ConversationClient::new_for_test());

        parent.register_child("child-1".to_string(), Arc::clone(&child));

        let parent_tool = parent.register_tool_token("pt");
        let child_tool = child.register_tool_token("ct");

        parent.cancel_silent();

        assert!(parent_tool.is_cancelled());
        assert!(child.current_cancel_token().is_cancelled());
        assert!(child_tool.is_cancelled());
    }

    // ======== tool denial wrapper text (build_tool_result_content) ========

    #[test]
    fn tool_denial_wrapper_matches_no_reason_text() {
        use crate::conversation::{MessageEndStatus, build_tool_result_content};

        let wrapped = build_tool_result_content(
            &MessageEndStatus::UserDenied,
            vec![ContentPart::Text("raw tool output".to_string())],
        );

        let text: String = wrapped
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        // Exact byte-equal text we send on the denial path. The reason (if
        // any) is already baked into the raw_content by ask_permission_inner,
        // so the wrapper itself is reason-agnostic. Uses the em-dash (U+2014)
        // and a single newline between the boilerplate and the original output.
        let expected = "The user denied permission for this tool call. This is not a technical error — \
                        the human operator chose not to allow this action. Do not retry this tool call. \
                        Instead, ask the user what they would like to do.\n\
                        Original tool output: raw tool output";
        assert_eq!(text, expected);
    }

    #[test]
    fn tool_denial_non_denied_status_passes_raw_content_through() {
        use crate::conversation::{MessageEndStatus, build_tool_result_content};

        for status in [
            MessageEndStatus::Succeeded,
            MessageEndStatus::Failed,
            MessageEndStatus::Cancelled,
            MessageEndStatus::Timeout,
        ] {
            let wrapped =
                build_tool_result_content(&status, vec![ContentPart::Text("hello".to_string())]);
            let text: String = wrapped
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            assert_eq!(text, "hello", "status {status:?} should be pass-through");
        }
    }

    // ======== LLM Retry + Timeout Tests ========

    enum MockResponse {
        /// Yield these events in order, then end.
        Events(Vec<LLMEvent>),
        /// Yield these events in order, then stall forever (never return None).
        EventsThenStall(Vec<LLMEvent>),
        /// Never yield (simulates a stalled stream -> timeout).
        Stall,
    }

    #[derive(Clone)]
    struct MockLlm {
        inner: Arc<Mutex<MockLlmInner>>,
    }

    struct MockLlmInner {
        responses: Vec<MockResponse>,
    }

    impl MockLlm {
        fn new(responses: Vec<MockResponse>) -> Self {
            Self {
                inner: Arc::new(Mutex::new(MockLlmInner { responses })),
            }
        }
    }

    impl crate::llm::LLM for MockLlm {
        fn register_tools(&mut self, _tools: Vec<Arc<Tool>>) {}

        fn chat(
            &self,
            _model: &str,
            _msgs: &[LLMMessage],
            _options: &ChatOptions,
        ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>> {
            let mut inner = self.inner.lock().unwrap();
            let response = inner.responses.remove(0);
            match response {
                MockResponse::Events(events) => Box::pin(tokio_stream::iter(events)),
                MockResponse::EventsThenStall(events) => {
                    Box::pin(tokio_stream::iter(events).chain(tokio_stream::pending()))
                }
                MockResponse::Stall => Box::pin(tokio_stream::pending()),
            }
        }

        fn clone_box(&self) -> Box<dyn crate::llm::LLM> {
            Box::new(self.clone())
        }

        fn set_media_dir(&mut self, _dir: Option<PathBuf>) {}

        fn available_models(&self) -> Vec<ModelInfo> {
            vec![]
        }
    }

    /// Drive a conversation with a MockLlm: send "Hello" and collect broadcast
    /// messages until AssistantMessageEnd (or 30s deadline).
    async fn drive_conversation(
        mock: MockLlm,
        chat_options: ChatOptions,
        dir: &Path,
    ) -> anyhow::Result<Vec<Arc<crate::conversation::BroadcastMessage>>> {
        use crate::conversation::{BroadcastMessage, Message};
        let permissions_file = dir.join("permissions.json");
        std::fs::write(&permissions_file, "[]")?;
        let manager = ConversationManager::new(permissions_file, None);

        let (_conv_id, client) = manager.new_conversation_with_id(
            "test-conv".to_string(),
            Box::new(mock),
            "test-model",
            vec![],
            chat_options,
            true, // single_turn
            0,    // subagent_depth
            10,   // max_subagent_depth
            Some(dir.to_path_buf()),
            false, // supports_media
        )?;

        let mut stream = client.subscribe();
        client.send_chat("Hello").await?;

        let mut messages: Vec<Arc<BroadcastMessage>> = Vec::new();
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(30));
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                msg = stream.next() => {
                    match msg {
                        Some(Ok(m)) => {
                            let is_end = matches!(&m.msg, Message::AssistantMessageEnd { .. });
                            messages.push(m);
                            if is_end {
                                break;
                            }
                        }
                        Some(Err(BroadcastStreamRecvError::Lagged(_))) => continue,
                        None => break,
                    }
                }
                _ = &mut deadline => {
                    break;
                }
            }
        }

        Ok(messages)
    }

    /// Extract a sorted, de-duplicated list of Message variant names for
    /// asserting the presence of expected variants.
    fn msg_variants(msgs: &[Arc<crate::conversation::BroadcastMessage>]) -> Vec<String> {
        let mut names: Vec<String> = msgs
            .iter()
            .map(|m| {
                let s = format!("{:?}", m.msg);
                s.split('{').next().unwrap_or(&s).trim().to_string()
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Count occurrences of a specific Message variant in the collected messages.
    fn count_msg<T: Fn(&crate::conversation::Message) -> bool>(
        msgs: &[Arc<crate::conversation::BroadcastMessage>],
        pred: T,
    ) -> usize {
        msgs.iter().filter(|m| pred(&m.msg)).count()
    }

    // ----------------------------------------------------------------
    // Test: timeout with no content -> retries then succeeds
    // ----------------------------------------------------------------
    #[tokio::test]
    async fn retry_timeout_no_content_then_succeeds() -> anyhow::Result<()> {
        use crate::conversation::{Message, MessageEndStatus};
        let dir = TestDir::new("conversation");
        let mock = MockLlm::new(vec![
            MockResponse::Stall, // attempt 0: timeout
            MockResponse::Stall, // attempt 1: timeout
            MockResponse::Events(vec![
                LLMEvent::TextDelta("Hello".to_string()),
                LLMEvent::MessageEnd {
                    stop_reason: StopReason::EndTurn,
                    input_tokens: 10,
                    output_tokens: 5,
                    reasoning_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    raw: None,
                },
            ]),
        ]);

        let chat_options = ChatOptions {
            request_timeout_secs: Some(1),
            max_retries: Some(2),
            ..Default::default()
        };

        let msgs = drive_conversation(mock, chat_options, dir.path()).await?;
        let variants = msg_variants(&msgs);

        assert!(
            variants.contains(&"AssistantMessageStart".to_string()),
            "Expected AssistantMessageStart in {variants:?}"
        );

        // Exactly 2 LLMRetry messages (attempts 1 and 2, both before backoff)
        let retry_count = count_msg(&msgs, |m| matches!(m, Message::LLMRetry { .. }));
        assert_eq!(
            retry_count, 2,
            "Expected 2 LLMRetry messages, got {retry_count}"
        );

        // Verify specific retry attempts
        let retry_attempts: Vec<u32> = msgs
            .iter()
            .filter_map(|m| match &m.msg {
                Message::LLMRetry { attempt, .. } => Some(*attempt),
                _ => None,
            })
            .collect();
        assert_eq!(retry_attempts, vec![1, 2], "Expected attempts 1 and 2");

        // Chunk with "Hello" content arrived on retry
        let has_hello = msgs.iter().any(|m| {
            matches!(&m.msg, Message::AssistantMessageChunk { content, .. } if content.as_ref() == "Hello")
        });
        assert!(has_hello, "Expected AssistantMessageChunk with 'Hello'");

        // Final status is Succeeded
        let end = msgs
            .iter()
            .find(|m| matches!(&m.msg, Message::AssistantMessageEnd { .. }));
        assert!(end.is_some(), "Expected AssistantMessageEnd");
        if let Some(m) = end {
            match &m.msg {
                Message::AssistantMessageEnd { end_status, .. } => {
                    assert_eq!(*end_status, MessageEndStatus::Succeeded);
                }
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    // ----------------------------------------------------------------
    // Test: timeout with no content -> all retries exhausted
    // ----------------------------------------------------------------
    #[tokio::test]
    async fn retry_timeout_no_content_all_exhausted() -> anyhow::Result<()> {
        use crate::conversation::{Message, MessageEndStatus};
        let dir = TestDir::new("conversation");
        // Always stalls -> timeout on every attempt
        let mock = MockLlm::new(vec![
            MockResponse::Stall,
            MockResponse::Stall,
            MockResponse::Stall,
        ]);

        let chat_options = ChatOptions {
            request_timeout_secs: Some(1),
            max_retries: Some(2),
            ..Default::default()
        };

        let msgs = drive_conversation(mock, chat_options, dir.path()).await?;
        let variants = msg_variants(&msgs);

        assert!(variants.contains(&"AssistantMessageStart".to_string()));

        // 2 LLMRetry messages (attempt 1 and 2)
        let retry_count = count_msg(&msgs, |m| matches!(m, Message::LLMRetry { .. }));
        assert_eq!(retry_count, 2, "Expected 2 LLMRetry messages");

        // No text chunks arrived
        let chunk_count = count_msg(&msgs, |m| {
            matches!(m, Message::AssistantMessageChunk { .. })
        });
        assert_eq!(chunk_count, 0, "Expected no AssistantMessageChunk");

        // Final status is Timeout
        let end = msgs
            .iter()
            .find(|m| matches!(&m.msg, Message::AssistantMessageEnd { .. }));
        assert!(end.is_some(), "Expected AssistantMessageEnd");
        if let Some(m) = end {
            match &m.msg {
                Message::AssistantMessageEnd { end_status, .. } => {
                    assert_eq!(*end_status, MessageEndStatus::Timeout);
                }
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    // ----------------------------------------------------------------
    // Test: timeout after partial content -> no retry
    // ----------------------------------------------------------------
    #[tokio::test]
    async fn retry_timeout_after_partial_content_no_retry() -> anyhow::Result<()> {
        use crate::conversation::{Message, MessageEndStatus};
        let dir = TestDir::new("conversation");
        // First call: yields one chunk then stalls (never ends)
        let mock = MockLlm::new(vec![MockResponse::EventsThenStall(vec![
            LLMEvent::TextDelta("Partial".to_string()),
            // Stream stalls after partial content (no MessageEnd, never returns None)
        ])]);

        let chat_options = ChatOptions {
            request_timeout_secs: Some(1),
            max_retries: Some(2),
            ..Default::default()
        };

        let msgs = drive_conversation(mock, chat_options, dir.path()).await?;
        let variants = msg_variants(&msgs);

        assert!(variants.contains(&"AssistantMessageStart".to_string()));

        // Chunk with "Partial" arrived
        let has_partial = msgs.iter().any(|m| {
            matches!(&m.msg, Message::AssistantMessageChunk { content, .. } if content.as_ref() == "Partial")
        });
        assert!(has_partial, "Expected chunk with 'Partial'");

        // No LLMRetry because content was already received
        let retry_count = count_msg(&msgs, |m| matches!(m, Message::LLMRetry { .. }));
        assert_eq!(retry_count, 0, "Expected no LLMRetry after partial content");

        // Final status is Timeout (no retry after content was seen)
        let end = msgs
            .iter()
            .find(|m| matches!(&m.msg, Message::AssistantMessageEnd { .. }));
        assert!(end.is_some(), "Expected AssistantMessageEnd");
        if let Some(m) = end {
            match &m.msg {
                Message::AssistantMessageEnd { end_status, .. } => {
                    assert_eq!(*end_status, MessageEndStatus::Timeout);
                }
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    // ----------------------------------------------------------------
    // Test: error with no content -> retries
    // ----------------------------------------------------------------
    #[tokio::test]
    async fn retry_error_no_content_retries() -> anyhow::Result<()> {
        use crate::conversation::{Message, MessageEndStatus};
        let dir = TestDir::new("conversation");
        let mock = MockLlm::new(vec![
            MockResponse::Events(vec![LLMEvent::Error(
                "500 Internal Server Error".to_string(),
            )]),
            MockResponse::Events(vec![
                LLMEvent::TextDelta("Recovered".to_string()),
                LLMEvent::MessageEnd {
                    stop_reason: StopReason::EndTurn,
                    input_tokens: 5,
                    output_tokens: 3,
                    reasoning_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    raw: None,
                },
            ]),
        ]);

        let chat_options = ChatOptions {
            request_timeout_secs: Some(5),
            max_retries: Some(1),
            ..Default::default()
        };

        let msgs = drive_conversation(mock, chat_options, dir.path()).await?;
        let variants = msg_variants(&msgs);

        assert!(variants.contains(&"AssistantMessageStart".to_string()));

        // One LLMRetry with the error reason
        let retry_count = count_msg(&msgs, |m| matches!(m, Message::LLMRetry { .. }));
        assert_eq!(retry_count, 1, "Expected 1 LLMRetry message");

        // Verify retry reason contains the error
        let has_error_reason = msgs.iter().any(|m| {
            matches!(&m.msg, Message::LLMRetry { reason, .. } if reason.contains("500 Internal Server Error"))
        });
        assert!(
            has_error_reason,
            "LLMRetry reason should contain the error message"
        );

        // Recovered on retry
        let has_recovered = msgs.iter().any(|m| {
            matches!(&m.msg, Message::AssistantMessageChunk { content, .. } if content.as_ref() == "Recovered")
        });
        assert!(has_recovered, "Expected chunk with 'Recovered'");

        // Final status is Succeeded
        let end = msgs
            .iter()
            .find(|m| matches!(&m.msg, Message::AssistantMessageEnd { .. }));
        if let Some(m) = end {
            match &m.msg {
                Message::AssistantMessageEnd { end_status, .. } => {
                    assert_eq!(*end_status, MessageEndStatus::Succeeded);
                }
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    // ----------------------------------------------------------------
    // Test: cancellation during backoff sleep
    // ----------------------------------------------------------------
    #[tokio::test]
    async fn retry_cancellation_during_backoff() -> anyhow::Result<()> {
        use crate::conversation::{BroadcastMessage, Message, MessageEndStatus};
        let dir = TestDir::new("conversation");
        // First call stalls, then the mock will be cancelled
        let mock = MockLlm::new(vec![
            MockResponse::Stall, // attempt 0: timeout
            MockResponse::Stall, // attempt 1: would stall again
        ]);

        let chat_options = ChatOptions {
            request_timeout_secs: Some(1),
            max_retries: Some(2),
            ..Default::default()
        };

        let permissions_file = dir.path().join("permissions.json");
        std::fs::write(&permissions_file, "[]")?;
        let manager = ConversationManager::new(permissions_file, None);

        let (_conv_id, client) = manager.new_conversation_with_id(
            "test-conv".to_string(),
            Box::new(mock),
            "test-model",
            vec![],
            chat_options,
            true,
            0,
            10,
            Some(dir.path().to_path_buf()),
            false,
        )?;

        let mut stream = client.subscribe();
        client.send_chat("Hello").await?;

        let mut messages: Vec<Arc<BroadcastMessage>> = Vec::new();
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(30));
        tokio::pin!(deadline);

        // Collect until we see the first LLMRetry, then cancel
        let mut saw_retry = false;
        loop {
            tokio::select! {
                msg = stream.next() => {
                    match msg {
                        Some(Ok(m)) => {
                            let is_retry = matches!(&m.msg, Message::LLMRetry { .. });
                            let is_end = matches!(&m.msg, Message::AssistantMessageEnd { .. });
                            messages.push(m);
                            if is_retry && !saw_retry {
                                saw_retry = true;
                                // Cancel the conversation during backoff sleep
                                client.cancel();
                            }
                            if is_end {
                                break;
                            }
                        }
                        Some(Err(BroadcastStreamRecvError::Lagged(_))) => continue,
                        None => break,
                    }
                }
                _ = &mut deadline => {
                    break;
                }
            }
        }

        // Should have seen LLMRetry
        let retry_count = count_msg(&messages, |m| matches!(m, Message::LLMRetry { .. }));
        assert!(
            retry_count >= 1,
            "Expected at least 1 LLMRetry, got {retry_count}"
        );

        // Final status should be Cancelled (not Timeout)
        let end = messages
            .iter()
            .find(|m| matches!(&m.msg, Message::AssistantMessageEnd { .. }));
        assert!(end.is_some(), "Expected AssistantMessageEnd");
        if let Some(m) = end {
            match &m.msg {
                Message::AssistantMessageEnd { end_status, .. } => {
                    assert_eq!(*end_status, MessageEndStatus::Cancelled);
                }
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    // ----------------------------------------------------------------
    // Test: max_retries = 0 disables retry entirely
    // ----------------------------------------------------------------
    #[tokio::test]
    async fn retry_max_retries_zero_disables_retry() -> anyhow::Result<()> {
        use crate::conversation::{Message, MessageEndStatus};
        let dir = TestDir::new("conversation");
        let mock = MockLlm::new(vec![MockResponse::Stall]);

        let chat_options = ChatOptions {
            request_timeout_secs: Some(1),
            max_retries: Some(0),
            ..Default::default()
        };

        let msgs = drive_conversation(mock, chat_options, dir.path()).await?;
        let variants = msg_variants(&msgs);

        assert!(variants.contains(&"AssistantMessageStart".to_string()));

        // No LLMRetry messages
        let retry_count = count_msg(&msgs, |m| matches!(m, Message::LLMRetry { .. }));
        assert_eq!(retry_count, 0, "Expected no LLMRetry with max_retries=0");

        // Immediate AssistantMessageEnd with Timeout
        let end = msgs
            .iter()
            .find(|m| matches!(&m.msg, Message::AssistantMessageEnd { .. }));
        assert!(end.is_some(), "Expected AssistantMessageEnd");
        if let Some(m) = end {
            match &m.msg {
                Message::AssistantMessageEnd { end_status, .. } => {
                    assert_eq!(*end_status, MessageEndStatus::Timeout);
                }
                _ => unreachable!(),
            }
        }

        Ok(())
    }

    // ----------------------------------------------------------------
    // Test: BroadcastMessage envelope serde round-trip
    // ----------------------------------------------------------------
    #[test]
    fn broadcast_message_serde_roundtrip() -> anyhow::Result<()> {
        use crate::conversation::{BroadcastMessage, Message, MessageEndStatus, UniqueId};
        // UniqueId has no public constructor by design; serde deserialization
        // (reading persisted events) is the sanctioned way to obtain one.
        let uid = |n: i64| -> anyhow::Result<UniqueId> {
            Ok(serde_json::from_value::<UniqueId>(serde_json::json!(n))?)
        };
        let envelopes = vec![
            BroadcastMessage {
                id: uid(7)?,
                msg: Message::UserMessage {
                    created_at: 123,
                    content: Arc::new("hello".to_string()),
                    media_filenames: vec!["uuid.png".to_string()],
                },
            },
            // Empty struct variants must survive the envelope round trip too.
            BroadcastMessage {
                id: uid(8)?,
                msg: Message::ConversationSaved {},
            },
            BroadcastMessage {
                id: uid(9)?,
                msg: Message::PermissionUpdated {},
            },
            // A large epoch-prefixed id round-trips exactly (i64).
            BroadcastMessage {
                id: uid((1i64 << 32) | 10)?,
                msg: Message::AssistantMessageEnd {
                    end_status: MessageEndStatus::Succeeded,
                    error: None,
                    input_tokens: 1,
                    output_tokens: 2,
                    reasoning_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    aggregate_input_tokens: 1,
                    aggregate_output_tokens: 2,
                    aggregate_cache_creation_tokens: 0,
                    aggregate_cache_read_tokens: 0,
                    tool_call_count: 0,
                },
            },
        ];
        for envelope in envelopes {
            let json = serde_json::to_string(&envelope)?;
            let parsed: BroadcastMessage = serde_json::from_str(&json)?;
            let json2 = serde_json::to_string(&parsed)?;
            assert_eq!(json, json2);
        }
        Ok(())
    }

    // ----------------------------------------------------------------
    // Test: legacy Message JSON (with msg_id) still parses as Message
    // ----------------------------------------------------------------
    #[test]
    fn legacy_message_json_still_parses() -> anyhow::Result<()> {
        use crate::conversation::Message;
        let legacy = r#"{"UserMessage": {"msg_id": 7, "created_at": 123, "content": "hi", "media_filenames": ["uuid.png"]}}"#;
        let msg: Message = serde_json::from_str(legacy)?;
        match msg {
            Message::UserMessage {
                created_at,
                content,
                media_filenames,
            } => {
                assert_eq!(created_at, 123);
                assert_eq!(content.as_str(), "hi");
                assert_eq!(media_filenames, vec!["uuid.png".to_string()]);
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    // ----------------------------------------------------------------
    // Test: broadcast ids are unique (never repeated), stream order matches
    // the replay order from get_messages()
    //
    // Ids are NOT contiguous and NOT ordered: they are minted with a Relaxed
    // atomic under concurrent workers, so the order in the stream may differ
    // from minting order. Only uniqueness is guaranteed (plan section 2.1).
    // ----------------------------------------------------------------
    #[tokio::test]
    async fn broadcast_ids_are_unique_and_match_replay_order() -> anyhow::Result<()> {
        use crate::conversation::{BroadcastMessage, Message, SystemMessageLevel};
        use std::collections::HashSet;
        let client = Arc::new(ConversationClient::new_for_test());
        let mut stream = client.subscribe();

        let tasks_per_worker = 25usize;
        let workers = 4usize;
        let total = tasks_per_worker * workers;

        // Spawn several workers broadcasting concurrently.
        let mut tasks = Vec::new();
        for w in 0..workers {
            let client = Arc::clone(&client);
            tasks.push(tokio::spawn(async move {
                for i in 0..tasks_per_worker {
                    let n = (w * tasks_per_worker + i) as u64;
                    client.notify_msg(Message::SystemMessage {
                        created_at: n,
                        level: SystemMessageLevel::Info,
                        message: format!("worker-{w}-msg-{n}"),
                    })?;
                }
                anyhow::Ok(())
            }));
        }

        // Collect everything the stream yields while the workers broadcast.
        let mut received: Vec<Arc<BroadcastMessage>> = Vec::new();
        while received.len() < total {
            match stream.next().await {
                Some(Ok(env)) => received.push(env),
                Some(Err(_)) => continue,
                None => break,
            }
        }
        for task in tasks {
            task.await??;
        }

        assert_eq!(
            received.len(),
            total,
            "stream should yield exactly {total} messages"
        );

        // (a) ids are unique, all in epoch 0 (< 2^32 for new_for_test), and
        // each envelope's payload matches its created_at (distinct payloads).
        let mut seen: HashSet<i64> = HashSet::new();
        for env in &received {
            let id = env.id.as_i64();
            assert!(
                (0..(1 << 32)).contains(&id),
                "new_for_test uses epoch 0, so ids must be in [0, 2^32), got {id}"
            );
            assert!(seen.insert(id), "id {id} minted twice");
            match &env.msg {
                Message::SystemMessage {
                    created_at,
                    message,
                    ..
                } => {
                    assert_eq!(
                        message,
                        &format!(
                            "worker-{}-msg-{created_at}",
                            created_at / tasks_per_worker as u64
                        )
                    );
                }
                _ => unreachable!(),
            }
        }

        // (b) get_messages() afterwards yields the same ids in the same order
        let replay = client.get_messages();
        assert_eq!(replay.len(), total);
        let received_ids: Vec<i64> = received.iter().map(|env| env.id.as_i64()).collect();
        let replay_ids: Vec<i64> = replay.iter().map(|env| env.id.as_i64()).collect();
        assert_eq!(
            received_ids, replay_ids,
            "replay order must match stream order"
        );

        Ok(())
    }

    // ======== UniqueId / UniqueIdGenerator (epoch-prefixed ids) ========

    #[test]
    fn unique_id_serde_roundtrip_as_json_number() -> anyhow::Result<()> {
        use crate::conversation::UniqueId;
        // UniqueId serializes as a plain JSON number (wire shape unchanged).
        let id: UniqueId = serde_json::from_str("7")?;
        assert_eq!(id.as_i64(), 7);
        assert_eq!(id.to_string(), "7");
        assert_eq!(serde_json::to_string(&id)?, "7");

        // A large epoch-prefixed id round-trips exactly (i64).
        let big: UniqueId = serde_json::from_str("4294967298")?; // epoch 1, counter 2
        assert_eq!(big.as_i64(), (1i64 << 32) | 2);
        assert_eq!(serde_json::to_string(&big)?, "4294967298");

        // Comparisons are on the raw value.
        assert!(id < big);
        assert_ne!(id, big);
        Ok(())
    }

    #[test]
    fn unique_id_generator_none_dir_is_epoch_zero() -> anyhow::Result<()> {
        use crate::conversation::UniqueIdGenerator;
        let generator = UniqueIdGenerator::new(None)?;
        assert_eq!(generator.epoch(), 0);
        for _ in 0..5 {
            let id = generator.get_unique_id();
            assert!(id.as_i64() < (1 << 32), "epoch 0 ids must be < 2^32");
        }
        Ok(())
    }

    #[test]
    fn unique_id_generator_creates_epoch_file_on_first_run() -> anyhow::Result<()> {
        use crate::conversation::UniqueIdGenerator;
        let dir = TestDir::new("conversation");

        let generator = UniqueIdGenerator::new(Some(dir.path()))?;
        assert_eq!(
            generator.epoch(),
            1,
            "new conversation (no epoch file) starts at epoch 1"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("msg-id-epoch"))?,
            "1\n"
        );
        // No leftover tmp file after the tmp+rename protocol.
        assert!(!dir.path().join("msg-id-epoch.tmp").exists());

        let ids: Vec<i64> = (0..3).map(|_| generator.get_unique_id().as_i64()).collect();
        assert!(
            ids.iter().all(|&id| id >= (1 << 32)),
            "epoch 1 ids must be >= 2^32"
        );
        Ok(())
    }

    #[test]
    fn resume_twice_bumps_epochs_and_never_repeats_ids() -> anyhow::Result<()> {
        use crate::conversation::UniqueIdGenerator;
        let dir = TestDir::new("conversation");

        // Run 1: new conversation -> epoch 1.
        let gen1 = UniqueIdGenerator::new(Some(dir.path()))?;
        assert_eq!(gen1.epoch(), 1);
        let ids1: Vec<i64> = (0..3).map(|_| gen1.get_unique_id().as_i64()).collect();

        // Run 2: first resume -> epoch 2.
        let gen2 = UniqueIdGenerator::new(Some(dir.path()))?;
        assert_eq!(gen2.epoch(), 2);
        let ids2: Vec<i64> = (0..3).map(|_| gen2.get_unique_id().as_i64()).collect();

        // Run 3: second resume -> epoch 3.
        let gen3 = UniqueIdGenerator::new(Some(dir.path()))?;
        assert_eq!(gen3.epoch(), 3);
        let ids3: Vec<i64> = (0..3).map(|_| gen3.get_unique_id().as_i64()).collect();

        // The epoch file now durably reads 3.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("msg-id-epoch"))?,
            "3\n"
        );

        let mut all = ids1.clone();
        all.extend(ids2.iter().copied());
        all.extend(ids3.iter().copied());
        let unique: std::collections::HashSet<i64> = all.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all.len(),
            "ids must never repeat across epochs"
        );
        assert!(
            all.iter().all(|&id| id >= (1 << 32)),
            "every minted id must be in epoch 1+"
        );
        Ok(())
    }

    #[test]
    fn legacy_session_without_epoch_file_resumes_to_epoch_1() -> anyhow::Result<()> {
        use crate::conversation::UniqueIdGenerator;
        let dir = TestDir::new("conversation");
        // Legacy session: old i32 ids persisted, no msg-id-epoch file.
        let old_ids: Vec<i64> = vec![0, 1, 2, 1_000_000]; // all < 2^32

        let generator = UniqueIdGenerator::new(Some(dir.path()))?;
        assert_eq!(
            generator.epoch(),
            1,
            "legacy (no epoch file) resumes to epoch 1"
        );

        let new_ids: Vec<i64> = (0..4).map(|_| generator.get_unique_id().as_i64()).collect();
        for &id in &new_ids {
            assert!(id >= (1 << 32), "new id {id} must be in epoch 1+");
            assert!(
                old_ids.iter().all(|&old| id > old),
                "new id {id} must exceed every legacy id {old_ids:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn synthetic_stale_close_ids_never_collide_with_later_broadcasts() -> anyhow::Result<()> {
        use crate::conversation::{Message, SystemMessageLevel};
        let dir = TestDir::new("conversation");
        // Pre-bump the epoch file to simulate a session that has already
        // resumed several times (the file is the only thing that matters).
        std::fs::write(dir.path().join("msg-id-epoch"), "5\n")?;

        let permissions_file = dir.path().join("permissions.json");
        std::fs::write(&permissions_file, "[]")?;
        let manager = ConversationManager::new(permissions_file, None);
        let (_conv_id, client) = manager.new_conversation_with_id(
            "test-conv".to_string(),
            Box::new(MockLlm::new(vec![])),
            "test-model",
            vec![],
            ChatOptions::default(),
            true,
            0,
            10,
            Some(dir.path().to_path_buf()),
            false,
        )?;
        assert_eq!(
            std::fs::read_to_string(dir.path().join("msg-id-epoch"))?,
            "6\n",
            "conversation start bumps the pre-written epoch 5 -> 6"
        );

        // Keep a live subscriber so notify_msg's broadcast send doesn't fail.
        let _stream = client.subscribe();

        // Synthetic stale-close events reserve ids directly from the
        // generator (the server's close_stale path) without broadcasting.
        let synthetic: Vec<i64> = (0..5).map(|_| client.get_unique_id().as_i64()).collect();

        // Later live broadcasts continue from the same counter.
        for i in 0..5 {
            client.notify_msg(Message::SystemMessage {
                created_at: i,
                level: SystemMessageLevel::Info,
                message: format!("live-{i}"),
            })?;
        }
        let live: Vec<i64> = client
            .get_messages()
            .iter()
            .map(|env| env.id.as_i64())
            .collect();

        let mut all = synthetic.clone();
        all.extend(live.iter().copied());
        let unique: std::collections::HashSet<i64> = all.iter().copied().collect();
        assert_eq!(
            unique.len(),
            all.len(),
            "no id may repeat, synthetic or live"
        );
        assert!(
            all.iter().all(|&id| id >= (6 << 32)),
            "every minted id must be in epoch 6"
        );
        Ok(())
    }

    #[test]
    fn crash_epoch_file_ahead_of_disk_skips_epoch_without_collision() -> anyhow::Result<()> {
        use crate::conversation::UniqueIdGenerator;
        let dir = TestDir::new("conversation");

        // Run 1: epoch 1, some ids are "written to disk".
        let gen1 = UniqueIdGenerator::new(Some(dir.path()))?;
        assert_eq!(gen1.epoch(), 1);
        let ids1: Vec<i64> = (0..3).map(|_| gen1.get_unique_id().as_i64()).collect();

        // Crash mid-run-2: the epoch file was durably bumped to 2 (step 3 of
        // the ordering protocol completed) but no epoch-2 events reached disk
        // (crash before the synthetic appends). Simulate by writing the epoch
        // file without appending any events.
        std::fs::write(dir.path().join("msg-id-epoch"), "2\n")?;

        // Next resume reads 2 -> uses epoch 3; epoch 2 is skipped, never
        // reused, so no id from run 1 can collide.
        let gen2 = UniqueIdGenerator::new(Some(dir.path()))?;
        assert_eq!(gen2.epoch(), 3, "epoch 2 must be skipped after the crash");
        let ids2: Vec<i64> = (0..3).map(|_| gen2.get_unique_id().as_i64()).collect();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("msg-id-epoch"))?,
            "3\n"
        );

        let mut all = ids1.clone();
        all.extend(ids2.iter().copied());
        let unique: std::collections::HashSet<i64> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "no id may repeat across the crash");
        assert!(
            all.iter().all(|&id| id >= (1 << 32)),
            "all ids must be epoch-prefixed"
        );
        Ok(())
    }
}
