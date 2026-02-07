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

    fn get_openai_key() -> Option<String> {
        std::env::var("OPENAI_API_KEY").ok()
    }

    fn get_openrouter_key() -> Option<String> {
        std::env::var("OPENROUTER_API_KEY").ok()
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
    async fn test_openai_basic_chat() {
        let Some(key) = get_openai_key() else {
            eprintln!("Skipping test_openai_basic_chat: OPENAI_API_KEY not set");
            return;
        };

        let client = OpenAI::new(&key);
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
    async fn test_openai_reasoning() {
        let Some(key) = get_openai_key() else {
            eprintln!("Skipping test_openai_reasoning: OPENAI_API_KEY not set");
            return;
        };

        let client = OpenAI::new(&key);
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

        // Check reasoning tokens > 0
        if let Some(LLMEvent::MessageEnd {
            stop_reason,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            reasoning,
        }) = events.iter().find(|e| matches!(e, LLMEvent::MessageEnd { .. }))
        {
            let thinking_count = events.iter().filter(|e| matches!(e, LLMEvent::ThinkingDelta(_))).count();
            assert!(
                *reasoning_tokens > 0,
                "Expected reasoning_tokens > 0, got 0. \
                 Usage: input={input_tokens}, output={output_tokens}, reasoning={reasoning_tokens}. \
                 Stop: {stop_reason:?}. ThinkingDelta events: {thinking_count}. \
                 Reasoning details count: {}",
                reasoning.len(),
            );
            assert!(
                !reasoning.is_empty(),
                "Expected reasoning details for round-tripping, got empty vec. \
                 reasoning_tokens={reasoning_tokens}, ThinkingDelta events: {thinking_count}",
            );
        } else {
            panic!("No MessageEnd event found, got: {summary}");
        }
    }

    #[tokio::test]
    async fn test_openai_tool_call() {
        let Some(key) = get_openai_key() else {
            eprintln!("Skipping test_openai_tool_call: OPENAI_API_KEY not set");
            return;
        };

        // Define a simple tool
        let tool = Tool::new::<EmptyParams, _, _, _, _>(
            "get_weather",
            "Get the current weather for a city",
            None,
            |_params: EmptyParams| {
                tokio_stream::once(Ok::<String, String>("Sunny, 22C".to_string()))
            },
        );

        let mut client = OpenAI::new(&key);
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

    // ========================================================================
    // OpenRouter (Chat Completions API) tests
    // ========================================================================

    #[tokio::test]
    async fn test_openrouter_basic_chat() {
        let Some(key) = get_openrouter_key() else {
            eprintln!("Skipping test_openrouter_basic_chat: OPENROUTER_API_KEY not set");
            return;
        };

        let client = OpenRouter::new(&key);
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
}
