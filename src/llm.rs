use std::pin::Pin;
use std::sync::Arc;
use tokio_stream::Stream;

pub struct Tool  {
}

pub enum LLMRole {
    System,
    User,
    Assistant,
    Tool,
}

pub trait LLM: Send + Sync {
    fn chat(&self, model: &str, tools: &Vec<Arc<Tool>>, msgs: &Vec<(LLMRole, String)>) -> Pin<Box<dyn Stream<Item = String> + Send>>;
}