//! Example: Simple chat with OpenAI/OpenRouter API
//!
//! Usage:
//!   OPENROUTER_API_KEY=your-key cargo run --example openai_chat
//!
//! Or for OpenAI:
//!   OPENAI_API_KEY=your-key OPENAI_BASE_URL=https://api.openai.com/v1 cargo run --example openai_chat

use std::collections::HashMap;
use std::env;
use std::io::{self, Write};

use llm_rs::llm::{LLMEvent, LLMRole, OpenAI, LLM};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() {
    // Try OpenRouter first, then fall back to OpenAI
    let (api_key, base_url, model) = if let Ok(key) = env::var("OPENROUTER_API_KEY") {
        (
            key,
            "https://openrouter.ai/api/v1".to_string(),
            env::var("MODEL").unwrap_or_else(|_| "openai/gpt-4o-mini".to_string()),
        )
    } else if let Ok(key) = env::var("OPENAI_API_KEY") {
        (
            key,
            env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            env::var("MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string()),
        )
    } else {
        eprintln!("Error: Set OPENROUTER_API_KEY or OPENAI_API_KEY environment variable");
        std::process::exit(1);
    };

    println!("Using model: {}", model);
    println!("Base URL: {}", base_url);
    println!();

    let client = OpenAI::new(&api_key, &base_url);

    // Simple conversation
    let messages = vec![
        (LLMRole::System, "You are a helpful assistant. Be concise.".to_string()),
        (LLMRole::User, "What is Rust programming language in 2 sentences?".to_string()),
    ];

    println!("User: What is Rust programming language in 2 sentences?");
    print!("Assistant: ");
    io::stdout().flush().unwrap();

    let tools = HashMap::new();
    let mut stream = client.chat(&model, &tools, &messages);

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
