//! Integration tests for OpenAI (Responses API) and OpenRouter (Chat Completions API).
//!
//! These tests require API keys set as environment variables:
//! - `OPENAI_API_KEY` for OpenAI tests (uses gpt-5-nano, cheapest reasoning model)
//! - `OPENROUTER_API_KEY` for OpenRouter tests
//!
//! Tests are skipped if the corresponding API key is not set.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_stream::StreamExt;

    use crate::llm::{
        ChatOptions, LLMEvent, LLMMessage, OpenAI, OpenRouter, ReasoningEffort, StopReason, LLM,
    };
    use crate::tool::Tool;

    /// Empty params struct for tool tests.
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct EmptyParams {}

    fn get_openai_key() -> String {
        std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set")
    }

    fn get_openrouter_key() -> String {
        std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY must be set")
    }

    /// Collect all events from a chat stream.
    async fn collect_events(
        mut stream: std::pin::Pin<Box<dyn tokio_stream::Stream<Item = LLMEvent> + Send>>,
    ) -> Vec<LLMEvent> {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    }

    /// Collect error messages from events.
    fn collect_errors(events: &[LLMEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|e| match e {
                LLMEvent::Error(msg) => Some(msg.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Summarize event types for assertion messages.
    fn event_summary(events: &[LLMEvent]) -> String {
        events
            .iter()
            .map(|e| match e {
                LLMEvent::MessageStart { .. } => "MessageStart",
                LLMEvent::TextDelta(_) => "TextDelta",
                LLMEvent::ThinkingDelta(_) => "ThinkingDelta",
                LLMEvent::ToolCall(_) => "ToolCall",
                LLMEvent::MessageEnd { .. } => "MessageEnd",
                LLMEvent::Error(_) => "Error",
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    // ========================================================================
    // OpenAI (Responses API) tests
    // ========================================================================

    #[tokio::test]
    #[ignore] // requires OPENAI_API_KEY
    async fn test_openai_basic_chat() {
        let client = OpenAI::new(get_openai_key());
        let messages = vec![
            LLMMessage::System("You are a helpful assistant. Be very concise.".to_string()),
            LLMMessage::User("Say hello in exactly 3 words.".to_string()),
        ];

        let stream = client.chat("gpt-5-nano", &messages, &ChatOptions::default());
        let events = collect_events(stream).await;

        // Should have at least MessageStart, some TextDelta, and MessageEnd
        let errors = collect_errors(&events);
        let summary = event_summary(&events);
        assert!(errors.is_empty(), "Unexpected errors: {errors:?}\nEvents: {summary}");

        assert!(events.iter().any(|e| matches!(e, LLMEvent::MessageStart { .. })),
            "Expected MessageStart event, got: {summary}");
        assert!(events.iter().any(|e| matches!(e, LLMEvent::TextDelta(_))),
            "Expected at least one TextDelta event, got: {summary}");
        assert!(events.iter().any(|e| matches!(e, LLMEvent::MessageEnd { .. })),
            "Expected MessageEnd event, got: {summary}");

        // Check MessageEnd has valid token counts
        if let Some(LLMEvent::MessageEnd {
            stop_reason,
            output_tokens,
            ..
        }) = events.iter().find(|e| matches!(e, LLMEvent::MessageEnd { .. }))
        {
            assert_eq!(*stop_reason, StopReason::EndTurn,
                "Expected EndTurn stop reason, got {:?}", stop_reason);
            assert!(*output_tokens > 0, "Expected output_tokens > 0, got {output_tokens}");
        }
    }

    #[tokio::test]
    #[ignore] // requires OPENAI_API_KEY
    async fn test_openai_reasoning() {
        let client = OpenAI::new(get_openai_key());
        let messages = vec![
            LLMMessage::System("You are a helpful assistant.".to_string()),
            LLMMessage::User("What is 17 * 23? Think step by step.".to_string()),
        ];

        let options = ChatOptions {
            reasoning_effort: Some(ReasoningEffort::Low),
            ..Default::default()
        };

        let stream = client.chat("gpt-5-nano", &messages, &options);
        let events = collect_events(stream).await;

        let errors = collect_errors(&events);
        let summary = event_summary(&events);
        assert!(errors.is_empty(), "API returned errors: {errors:?}\nEvents: {summary}");

        // With reasoning enabled, we should get ThinkingDelta events
        assert!(events.iter().any(|e| matches!(e, LLMEvent::ThinkingDelta(_))),
            "Expected ThinkingDelta events with reasoning enabled, got: {summary}");
        assert!(events.iter().any(|e| matches!(e, LLMEvent::TextDelta(_))),
            "Expected TextDelta events, got: {summary}");

        // Check raw is present for round-tripping and contains reasoning
        if let Some(LLMEvent::MessageEnd {
            stop_reason: _,
            input_tokens: _,
            output_tokens: _,
            reasoning_tokens: _,
            raw,
        }) = events.iter().find(|e| matches!(e, LLMEvent::MessageEnd { .. }))
        {
            let thinking_count = events.iter().filter(|e| matches!(e, LLMEvent::ThinkingDelta(_))).count();
            assert!(
                thinking_count > 0,
                "Expected ThinkingDelta events with reasoning enabled, got 0",
            );
            assert!(
                raw.is_some(),
                "Expected raw output for round-tripping, got None. \
                 ThinkingDelta events: {thinking_count}",
            );
            // Verify raw contains an array with reasoning item
            if let Some(raw_value) = raw {
                assert!(
                    raw_value.is_array(),
                    "Expected raw to be an array, got: {raw_value:?}"
                );
                let arr = raw_value.as_array().unwrap();
                assert!(
                    !arr.is_empty(),
                    "Expected raw array to have items for round-tripping"
                );
                // Check that at least one item is a reasoning item
                let has_reasoning = arr.iter().any(|item| {
                    item.get("type").and_then(|t| t.as_str()) == Some("reasoning")
                });
                assert!(
                    has_reasoning,
                    "Expected raw to contain a reasoning item for round-tripping, got: {raw_value:?}"
                );
            }
        } else {
            panic!("No MessageEnd event found, got: {summary}");
        }
    }

    #[tokio::test]
    #[ignore] // requires OPENAI_API_KEY
    async fn test_openai_tool_call() {
        // Define a simple tool
        let tool = Tool::new::<EmptyParams, _, _, _, _>(
            "get_weather",
            "Get the current weather for a city",
            None,
            |_ctx: crate::tool::ToolContext, _params: EmptyParams| {
                tokio_stream::once(Ok::<String, String>("Sunny, 22C".to_string()))
            },
        );

        let mut client = OpenAI::new(get_openai_key());
        client.register_tools(vec![Arc::new(tool)]);

        let messages = vec![LLMMessage::User(
            "What's the weather? Use the get_weather tool.".to_string(),
        )];

        let stream = client.chat("gpt-5-nano", &messages, &ChatOptions::default());
        let events = collect_events(stream).await;

        let errors = collect_errors(&events);
        let summary = event_summary(&events);
        assert!(errors.is_empty(), "API returned errors: {errors:?}\nEvents: {summary}");

        assert!(events.iter().any(|e| matches!(e, LLMEvent::ToolCall(_))),
            "Expected a ToolCall event, got: {summary}");

        // Check the tool call has the right name
        if let Some(LLMEvent::ToolCall(tc)) =
            events.iter().find(|e| matches!(e, LLMEvent::ToolCall(_)))
        {
            assert_eq!(tc.name, "get_weather",
                "Expected tool call to 'get_weather', got '{}'", tc.name);
        }

        // Check stop reason is ToolUse
        if let Some(LLMEvent::MessageEnd { stop_reason, .. }) =
            events.iter().find(|e| matches!(e, LLMEvent::MessageEnd { .. }))
        {
            assert_eq!(*stop_reason, StopReason::ToolUse,
                "Expected ToolUse stop reason, got {:?}", stop_reason);
        } else {
            panic!("No MessageEnd event found, got: {summary}");
        }
    }

    #[tokio::test]
    #[ignore] // requires OPENAI_API_KEY
    async fn test_openai_reasoning_multi_turn() {
        let client = OpenAI::new(get_openai_key());
        let options = ChatOptions {
            reasoning_effort: Some(ReasoningEffort::Low),
            ..Default::default()
        };

        // First turn: ask a question that requires reasoning
        let messages = vec![
            LLMMessage::System("You are a helpful assistant.".to_string()),
            LLMMessage::User("What is 7 + 5?".to_string()),
        ];

        let stream = client.chat("gpt-5-nano", &messages, &options);
        let events = collect_events(stream).await;

        let errors = collect_errors(&events);
        let summary = event_summary(&events);
        assert!(errors.is_empty(), "First turn errors: {errors:?}\nEvents: {summary}");

        // Extract the response for round-tripping
        let mut accumulated_text = String::new();
        let mut raw: Option<serde_json::Value> = None;

        for event in &events {
            match event {
                LLMEvent::TextDelta(text) => accumulated_text.push_str(text),
                LLMEvent::MessageEnd { raw: r, .. } => raw = r.clone(),
                _ => {}
            }
        }

        // Verify first turn has reasoning in raw for round-tripping
        assert!(raw.is_some(), "Expected raw for round-tripping, got None");
        let raw_value = raw.as_ref().unwrap();
        assert!(raw_value.is_array(), "Expected raw to be an array");
        let has_reasoning = raw_value.as_array().unwrap().iter().any(|item| {
            item.get("type").and_then(|t| t.as_str()) == Some("reasoning")
        });
        assert!(
            has_reasoning,
            "First turn raw must contain reasoning item for round-trip test. Got: {raw_value:?}"
        );

        // Second turn: continue the conversation with the raw response containing reasoning
        // If reasoning items aren't properly paired with their output items, OpenAI returns:
        // "Item 'rs_...' of type 'reasoning' was provided without its required following item."
        let messages = vec![
            LLMMessage::System("You are a helpful assistant.".to_string()),
            LLMMessage::User("What is 7 + 5?".to_string()),
            LLMMessage::Assistant {
                content: accumulated_text,
                tool_calls: vec![],
                raw,
            },
            LLMMessage::User("Now multiply that by 2.".to_string()),
        ];

        let stream = client.chat("gpt-5-nano", &messages, &options);
        let events = collect_events(stream).await;

        let errors = collect_errors(&events);
        let summary = event_summary(&events);
        assert!(
            errors.is_empty(),
            "Second turn failed (reasoning round-trip issue): {errors:?}\nEvents: {summary}"
        );

        assert!(
            events.iter().any(|e| matches!(e, LLMEvent::MessageEnd { .. })),
            "Expected MessageEnd in second turn, got: {summary}"
        );
    }

    // ========================================================================
    // OpenRouter (Chat Completions API) tests
    // ========================================================================

    #[tokio::test]
    #[ignore] // requires OPENROUTER_API_KEY
    async fn test_openrouter_basic_chat() {
        let client = OpenRouter::new(get_openrouter_key());
        let messages = vec![
            LLMMessage::System("You are a helpful assistant. Be very concise.".to_string()),
            LLMMessage::User("Say hello in exactly 3 words.".to_string()),
        ];

        // Use a cheap model
        let stream = client.chat(
            "deepseek/deepseek-chat",
            &messages,
            &ChatOptions::default(),
        );
        let events = collect_events(stream).await;

        let errors = collect_errors(&events);
        let summary = event_summary(&events);
        assert!(errors.is_empty(), "Unexpected errors: {errors:?}\nEvents: {summary}");

        assert!(events.iter().any(|e| matches!(e, LLMEvent::MessageStart { .. })),
            "Expected MessageStart event, got: {summary}");
        assert!(events.iter().any(|e| matches!(e, LLMEvent::TextDelta(_))),
            "Expected at least one TextDelta event, got: {summary}");
        assert!(events.iter().any(|e| matches!(e, LLMEvent::MessageEnd { .. })),
            "Expected MessageEnd event, got: {summary}");
    }

    #[tokio::test]
    #[ignore] // requires OPENROUTER_API_KEY
    async fn test_openrouter_multi_turn() {
        let client = OpenRouter::new(get_openrouter_key());

        // First turn
        let messages = vec![
            LLMMessage::System("You are a helpful assistant. Be concise.".to_string()),
            LLMMessage::User("What is 3 + 4?".to_string()),
        ];

        let stream = client.chat("deepseek/deepseek-chat", &messages, &ChatOptions::default());
        let events = collect_events(stream).await;

        let errors = collect_errors(&events);
        let summary = event_summary(&events);
        assert!(errors.is_empty(), "First turn errors: {errors:?}\nEvents: {summary}");

        // Extract response for round-tripping
        let mut accumulated_text = String::new();
        let mut raw: Option<serde_json::Value> = None;

        for event in &events {
            match event {
                LLMEvent::TextDelta(text) => accumulated_text.push_str(text),
                LLMEvent::MessageEnd { raw: r, .. } => raw = r.clone(),
                _ => {}
            }
        }

        assert!(raw.is_some(), "Expected raw for round-tripping");

        // Second turn with raw response
        let messages = vec![
            LLMMessage::System("You are a helpful assistant. Be concise.".to_string()),
            LLMMessage::User("What is 3 + 4?".to_string()),
            LLMMessage::Assistant {
                content: accumulated_text,
                tool_calls: vec![],
                raw,
            },
            LLMMessage::User("Multiply that by 2.".to_string()),
        ];

        let stream = client.chat("deepseek/deepseek-chat", &messages, &ChatOptions::default());
        let events = collect_events(stream).await;

        let errors = collect_errors(&events);
        let summary = event_summary(&events);
        assert!(errors.is_empty(), "Second turn errors: {errors:?}\nEvents: {summary}");

        assert!(
            events.iter().any(|e| matches!(e, LLMEvent::MessageEnd { .. })),
            "Expected MessageEnd in second turn, got: {summary}"
        );
    }
}
