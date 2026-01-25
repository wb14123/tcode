//! Example: Simple chat with OpenAI API
//!
//! Usage:
//!   OPENAI_API_KEY=your-key cargo run --example openai_chat

use std::env;
use std::io::{self, Write};

use llm_rs::llm::{LLMEvent, LLMRole, OpenAI, LLM};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() {
    let api_key = env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let model = "gpt-4o-mini";

    let client = OpenAI::new(&api_key, "https://api.openai.com/v1");

    // Simple conversation
    let messages = vec![
        (LLMRole::System, "You are a helpful assistant. Be concise.".to_string()),
        (LLMRole::User, "What is Rust programming language in 2 sentences?".to_string()),
    ];

    println!("User: What is Rust programming language in 2 sentences?");
    print!("Assistant: ");
    io::stdout().flush().unwrap();

    let mut stream = client.chat(&model, &messages);

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
            LLMEvent::ToolCall(tool_call) => {
                println!("\n[Tool call: {} with args: {}]", tool_call.name, tool_call.arguments);
            }
            LLMEvent::MessageEnd {
                stop_reason,
                input_tokens,
                output_tokens,
            } => {
                total_input += input_tokens;
                total_output += output_tokens;
                println!();
                println!();
                println!("--- Done ---");
                println!("Stop reason: {:?}", stop_reason);
                println!("Tokens: {} input, {} output", total_input, total_output);
            }
            LLMEvent::Error(err) => {
                eprintln!("\nError: {}", err);
            }
        }
    }
}
