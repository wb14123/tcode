use std::pin::Pin;
use tokio_stream::Stream;

pub struct Tool  {
}

pub enum LLMRole {
    System,
    User,
    Assistant,
    Tool,
}

pub trait LLM {
    fn chat(&self, msgs: Vec<(LLMRole, &String)>) -> Pin<Box<dyn Stream<Item = String> + Send>>;
}