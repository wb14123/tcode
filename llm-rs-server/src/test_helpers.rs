//! Shared test utilities.

use std::pin::Pin;
use std::sync::Arc;

use llm_rs::llm::{ChatOptions, LLM, LLMEvent, LLMMessage, ModelInfo};
use llm_rs::tool::Tool;
use tokio_stream::Stream;

/// A mock LLM that returns a fixed sequence of events.
pub struct MockLLM {
    pub events: Vec<LLMEvent>,
}

impl LLM for MockLLM {
    fn register_tools(&mut self, _tools: Vec<Arc<Tool>>) {}

    fn chat(
        &self,
        _model: &str,
        _msgs: &[LLMMessage],
        _options: &ChatOptions,
    ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>> {
        let events = self.events.clone();
        Box::pin(tokio_stream::iter(events))
    }

    fn clone_box(&self) -> Box<dyn LLM> {
        Box::new(MockLLM {
            events: self.events.clone(),
        })
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo {
            id: "mock-model".to_string(),
            description: "A mock model for testing".to_string(),
        }]
    }
}
