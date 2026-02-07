//! Example: Reasoning/thinking tokens with OpenRouter
//!
//! OpenRouter exposes the actual thinking text from reasoning models,
//! unlike OpenAI's Chat Completions API which only reports token counts.
//!
//! Usage:
//!   OPENROUTER_API_KEY=your-key cargo run --example openrouter_reasoning

use std::env;
use std::io::{self, Write};

use llm_rs::llm::{ChatOptions, LLMEvent, LLMMessage, OpenRouter, ReasoningEffort, LLM};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() {
    let api_key = env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY must be set");
    let model = "deepseek/deepseek-r1";

    let client = OpenRouter::new(&api_key);

    let messages = vec![
        LLMMessage::System("You are a helpful assistant. Be concise.".to_string()),
        LLMMessage::User("What is Rust programming language in 2 sentences?".to_string()),
    ];

    let chat_options = ChatOptions {
        reasoning_effort: Some(ReasoningEffort::Medium),
        ..Default::default()
    };

    println!("Model: {}", model);
    println!("Reasoning effort: Medium");
    println!();
    println!("User: What is Rust programming language in 2 sentences?");
    println!();

    let mut stream = client.chat(model, &messages, &chat_options);

    let mut total_input = 0;
    let mut total_output = 0;
    let mut in_thinking = false;

    while let Some(event) = stream.next().await {
        match event {
            LLMEvent::MessageStart { input_tokens } => {
                total_input += input_tokens;
            }
            LLMEvent::ThinkingDelta(text) => {
                if !in_thinking {
                    print!("\x1b[2m<thinking> ");
                    in_thinking = true;
                }
                print!("\x1b[2m{}\x1b[0m", text);
                io::stdout().flush().unwrap();
            }
            LLMEvent::TextDelta(text) => {
                if in_thinking {
                    println!("\x1b[2m </thinking>\x1b[0m");
                    println!();
                    in_thinking = false;
                }
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
                reasoning_tokens,
                ..
            } => {
                if in_thinking {
                    println!("\x1b[2m </thinking>\x1b[0m");
                    in_thinking = false;
                }
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
