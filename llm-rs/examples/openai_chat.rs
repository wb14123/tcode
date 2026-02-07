//! Example: Simple chat with OpenAI API (with reasoning/thinking support)
//!
//! Usage:
//!   OPENAI_API_KEY=your-key cargo run --example openai_chat

use std::env;
use std::io::{self, Write};

use llm_rs::llm::{ChatOptions, LLMEvent, LLMMessage, OpenAI, ReasoningEffort, LLM};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() {
    let api_key = env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let model = "gpt-5-nano";

    let client = OpenAI::new(&api_key);

    // Simple conversation
    let messages = vec![
        LLMMessage::System("You are a helpful assistant. Be concise.".to_string()),
        LLMMessage::User("What is Rust programming language in 2 sentences? Think deeply.".to_string()),
    ];

    let chat_options = ChatOptions {
        reasoning_effort: Some(ReasoningEffort::Medium),
        ..Default::default()
    };

    println!("User: What is Rust programming language in 2 sentences?");
    print!("Assistant: ");
    io::stdout().flush().unwrap();

    let mut stream = client.chat(model, &messages, &chat_options);

    let mut total_input = 0;
    let mut total_output = 0;

    while let Some(event) = stream.next().await {
        match event {
            LLMEvent::MessageStart { input_tokens } => {
                total_input += input_tokens;
            }
            LLMEvent::TextDelta(text) => {
                print!("{}", text);
                io::stdout().flush().unwrap();
            }
            LLMEvent::ThinkingDelta(text) => {
                // Dim color for thinking text
                print!("\x1b[2m{}\x1b[0m", text);
                io::stdout().flush().unwrap();
            }
            LLMEvent::ToolCall(tool_call) => {
                println!("\n[Tool call: {} with args: {}]", tool_call.name, tool_call.arguments);
            }
            LLMEvent::MessageEnd {
                stop_reason,
                input_tokens,
                output_tokens,
                reasoning_tokens,
                ..
            } => {
                total_input += input_tokens;
                total_output += output_tokens;
                println!();
                println!();
                println!("--- Done ---");
                println!("Stop reason: {:?}", stop_reason);
                println!("Tokens: {} input, {} output ({} reasoning)", total_input, total_output, reasoning_tokens);
            }
            LLMEvent::Error(err) => {
                eprintln!("\nError: {}", err);
            }
        }
    }
}
