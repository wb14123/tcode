//! Example: Multi-round conversation using ConversationManager
//!
//! This example demonstrates how to:
//! - Create a ConversationManager
//! - Start a conversation with tools
//! - Subscribe to conversation messages
//! - Send multiple chat messages in a conversation
//!
//! Usage:
//!   OPENAI_API_KEY=your-key cargo run --example conversation

use std::env;
use std::io::{self, Write};
use std::sync::Arc;

use llm_rs::conversation::{ConversationManager, Message};
use llm_rs::llm::OpenAI;
use llm_rs::tool;
use tokio_stream::StreamExt;

/// Get the current weather for a city
#[tool]
fn get_weather(
    /// The city name to get weather for
    city: String,
) -> impl tokio_stream::Stream<Item = String> {
    let result = format!(
        "Weather in {}: 22°C, partly cloudy, humidity 65%",
        city
    );
    tokio_stream::once(result)
}

/// Get the current time in a timezone
#[tool]
fn get_current_time(
    /// The timezone (e.g., "UTC", "Asia/Tokyo")
    timezone: String,
) -> impl tokio_stream::Stream<Item = String> {
    let now = chrono::Local::now();
    tokio_stream::once(format!(
        "Current time in {}: {}",
        timezone,
        now.format("%Y-%m-%d %H:%M:%S")
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let model = "gpt-4o-mini";

    // Create tools
    let tools = vec![
        Arc::new(get_weather_tool()),
        Arc::new(get_current_time_tool()),
    ];

    // Create conversation manager and start a conversation
    let manager = ConversationManager::new();
    let llm = Box::new(OpenAI::new(&api_key, "https://api.openai.com/v1"));

    let client = manager.new_conversation(
        llm,
        "You are a helpful assistant. Use tools when needed to answer questions about weather and time.",
        model,
        tools,
    )?;

    // Subscribe to messages (this must be done before sending to receive all messages)
    let mut msg_stream = client.subscribe();

    // Spawn a task to print messages as they arrive
    let print_task = tokio::spawn(async move {
        while let Some(result) = msg_stream.next().await {
            match result {
                Ok(msg) => print_message(&msg),
                Err(e) => eprintln!("[Stream error: {:?}]", e),
            }
        }
    });

    // Multi-round conversation
    let questions = [
        "What's the weather in Tokyo?",
        "And what about New York?",
        "What time is it in UTC?",
    ];

    for question in questions {
        println!("\n>>> User: {}", question);
        client.send_chat(question).await?;

        // Wait a bit for the response to complete before sending next message
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }

    // Give some time for the final response
    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

    // Cancel the print task
    print_task.abort();

    println!("\n\n--- Conversation ended ---");

    Ok(())
}

fn print_message(msg: &Message) {
    match msg {
        Message::UserMessage { content, .. } => {
            // Already printed by the main loop
            let _ = content;
        }
        Message::AssistantMessageStart { .. } => {
            print!("\n<<< Assistant: ");
            io::stdout().flush().unwrap();
        }
        Message::AssistantMessageChunk { content, .. } => {
            print!("{}", content);
            io::stdout().flush().unwrap();
        }
        Message::AssistantMessageEnd {
            input_tokens,
            output_tokens,
            ..
        } => {
            println!(
                "\n    [tokens: {} in, {} out]",
                input_tokens, output_tokens
            );
        }
        Message::ToolMessageStart {
            tool_name,
            tool_args,
            ..
        } => {
            println!("\n    [Tool: {} with args: {}]", tool_name, tool_args);
        }
        Message::ToolOutputChunk {
            tool_name, content, ..
        } => {
            println!("    [Tool {} output: {}]", tool_name, content);
        }
        Message::ToolMessageEnd { .. } => {
            println!("    [Tool execution completed]");
        }
        Message::SubAgentStart { description, .. } => {
            println!("\n    [SubAgent started: {}]", description);
        }
        Message::SubAgentEnd { .. } => {
            println!("    [SubAgent ended]");
        }
        Message::AssistantRequestEnd {
            total_input_tokens,
            total_output_tokens,
        } => {
            println!(
                "\n--- Total tokens: {} input, {} output ---",
                total_input_tokens, total_output_tokens
            );
        }
    }
}
