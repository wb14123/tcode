//! Example: Interactive multi-round conversation using ConversationManager
//!
//! This example demonstrates how to:
//! - Create a ConversationManager
//! - Start a conversation with tools
//! - Subscribe to conversation messages
//! - Send interactive user messages in a conversation
//!
//! Usage:
//!   OPENROUTER_API_KEY=your-key cargo run --example conversation

use std::env;
use std::io::{self, Write};
use std::sync::Arc;

use llm_rs::conversation::{ConversationManager, Message};
use llm_rs::llm::{ChatOptions, OpenRouter, ReasoningEffort};
use llm_rs::tool;
use tokio_stream::StreamExt;

/// Get the current weather for a city
#[tool]
fn get_weather(
    /// The city name to get weather for
    city: String,
) -> impl tokio_stream::Stream<Item = Result<String, String>> {
    let result = format!(
        "Weather in {}: 22°C, partly cloudy, humidity 65%",
        city
    );
    tokio_stream::once(Ok(result))
}

/// Get the current time in a timezone
#[tool]
fn get_current_time(
    /// The timezone (e.g., "UTC", "Asia/Tokyo")
    timezone: String,
) -> impl tokio_stream::Stream<Item = Result<String, String>> {
    let now = chrono::Local::now();
    tokio_stream::once(Ok(format!(
        "Current time in {}: {}",
        timezone,
        now.format("%Y-%m-%d %H:%M:%S")
    )))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY must be set");
    let model = "deepseek/deepseek-r1";

    // Create tools
    let tools = vec![
        Arc::new(get_weather_tool()),
        Arc::new(get_current_time_tool()),
    ];

    // Create conversation manager and start a conversation
    let manager = ConversationManager::new();
    let llm = Box::new(OpenRouter::new(&api_key));

    let (_, client) = manager.new_conversation(
        llm,
        "You are a helpful assistant. Use tools when needed to answer questions about weather and time.",
        model,
        tools,
        ChatOptions {
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..Default::default()
        },
        false,
        20,
    )?;

    // Subscribe to messages (this must be done before sending to receive all messages)
    let mut msg_stream = client.subscribe();

    // Spawn a task to print messages as they arrive
    tokio::spawn(async move {
        while let Some(result) = msg_stream.next().await {
            match result {
                Ok(msg) => print_message(&msg),
                Err(e) => eprintln!("[Stream error: {:?}]", e),
            }
        }
    });

    // Print welcome message and hints
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║       Interactive Conversation with AI Assistant           ║");
    println!("╠════════════════════════════════════════════════════════════╣");
    println!("║ Available tools:                                           ║");
    println!("║   • get_weather <city>  - Get weather for a city           ║");
    println!("║   • get_current_time <timezone> - Get time in a timezone   ║");
    println!("║                                                            ║");
    println!("║ Example questions:                                         ║");
    println!("║   • \"What's the weather in Tokyo?\"                         ║");
    println!("║   • \"What time is it in UTC?\"                              ║");
    println!("║                                                            ║");
    println!("║ Type 'quit' or 'exit' to end the conversation.             ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();

    // Interactive conversation loop
    loop {
        print!("You: ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        // Check for exit commands
        if input.is_empty() {
            continue;
        }
        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            println!("\n--- Conversation ended. Goodbye! ---");
            break;
        }

        // Send the message
        client.send_chat(input).await?;

        // Wait for the response to complete
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        println!();
    }

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
        Message::AssistantThinkingChunk { content, .. } => {
            print!("[thinking: {}]", content);
            io::stdout().flush().unwrap();
        }
        Message::AssistantMessageEnd {
            input_tokens,
            output_tokens,
            reasoning_tokens,
            ..
        } => {
            println!(
                "\n    [tokens: {} in, {} out, {} reasoning]",
                input_tokens, output_tokens, reasoning_tokens
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
        Message::SubAgentEnd { response, input_tokens, output_tokens, .. } => {
            println!("    [SubAgent ended: {} chars, {} in / {} out tokens]", response.len(), input_tokens, output_tokens);
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
        Message::SystemMessage { message, .. } => {
            println!("    [System: {}]", message);
        }
    }
}
