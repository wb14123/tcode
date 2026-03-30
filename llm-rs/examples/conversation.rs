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
    let result = format!("Weather in {}: 22°C, partly cloudy, humidity 65%", city);
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
    let manager =
        ConversationManager::new(std::env::temp_dir().join("llm-rs-example-permissions.json"));
    let llm = Box::new(OpenRouter::new(&api_key));

    let (_, client) = manager.new_conversation(
        llm,
        model,
        tools,
        ChatOptions {
            reasoning_effort: Some(ReasoningEffort::Medium),
            ..Default::default()
        },
        false,
        20,
        0,    // subagent_depth (root)
        3,    // max_subagent_depth
        None, // state_dir (no persistence for this example)
    )?;

    // Subscribe to messages (this must be done before sending to receive all messages)
    let mut msg_stream = client.subscribe();

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

        // Drive the stream until the full LLM turn is complete
        while let Some(result) = msg_stream.next().await {
            match result {
                Ok(msg) => {
                    let done = matches!(*msg, Message::AssistantRequestEnd { .. });
                    print_message(&msg);
                    if done {
                        break;
                    }
                }
                Err(e) => eprintln!("[Stream error: {:?}]", e),
            }
        }
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
            io::stdout().flush().ok();
        }
        Message::AssistantMessageChunk { content, .. } => {
            print!("{}", content);
            io::stdout().flush().ok();
        }
        Message::AssistantThinkingChunk { content, .. } => {
            print!("[thinking: {}]", content);
            io::stdout().flush().ok();
        }
        Message::AssistantMessageEnd {
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            ..
        } => {
            let new_input = input_tokens + cache_creation_input_tokens;
            let cache_info = if *cache_read_input_tokens > 0 {
                format!(" ({} cached)", cache_read_input_tokens)
            } else {
                String::new()
            };
            println!(
                "\n    [tokens: {} in{}, {} out, {} reasoning]",
                new_input, cache_info, output_tokens, reasoning_tokens
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
        Message::SubAgentEnd {
            response,
            input_tokens,
            output_tokens,
            ..
        } => {
            println!(
                "    [SubAgent ended: {} chars, {} in / {} out tokens]",
                response.len(),
                input_tokens,
                output_tokens
            );
        }
        Message::SubAgentTurnEnd {
            response,
            input_tokens,
            output_tokens,
            ..
        } => {
            println!(
                "    [SubAgent turn ended: {} chars, {} in / {} out tokens]",
                response.len(),
                input_tokens,
                output_tokens
            );
        }
        Message::SubAgentContinue { description, .. } => {
            println!("    [SubAgent continued: {}]", description);
        }
        Message::AssistantRequestEnd {
            total_input_tokens,
            total_output_tokens,
            ..
        } => {
            println!(
                "\n--- Total tokens: {} input, {} output ---",
                total_input_tokens, total_output_tokens
            );
        }
        Message::SystemMessage { message, .. } => {
            println!("    [System: {}]", message);
        }
        Message::UserRequestEnd {
            conversation_id, ..
        } => {
            println!("    [UserRequestEnd: {}]", conversation_id);
        }
        Message::ToolCallResolved { tool_call_id, .. } => {
            println!("    [ToolCallResolved: {}]", tool_call_id);
        }
        Message::PermissionUpdated { .. } => {
            println!("    [PermissionUpdated]");
        }
        Message::ToolRequestPermission { tool_call_id, .. } => {
            println!("    [ToolRequestPermission: {}]", tool_call_id);
        }
        Message::ToolPermissionApproved { tool_call_id, .. } => {
            println!("    [ToolPermissionApproved: {}]", tool_call_id);
        }
        Message::SubAgentWaitingPermission {
            conversation_id, ..
        } => {
            println!("    [SubAgentWaitingPermission: {}]", conversation_id);
        }
        Message::SubAgentPermissionApproved {
            conversation_id, ..
        } => {
            println!("    [SubAgentPermissionApproved: {}]", conversation_id);
        }
        Message::SubAgentPermissionDenied {
            conversation_id, ..
        } => {
            println!("    [SubAgentPermissionDenied: {}]", conversation_id);
        }
        Message::AssistantToolCallStart { tool_name, .. } => {
            print!("\n    [Tool call starting: {}]", tool_name);
            io::stdout().flush().ok();
        }
        Message::AssistantToolCallArgChunk { content, .. } => {
            print!("{}", content);
            io::stdout().flush().ok();
        }
        Message::SubAgentInputStart { tool_name, .. } => {
            eprintln!("  [subagent input start: {}]", tool_name);
        }
        Message::SubAgentInputChunk { content, .. } => {
            eprint!("{}", content);
        }
        Message::SubAgentTokenRollup { .. } => {}
        Message::AggregateTokenUpdate {
            aggregate_input_tokens,
            aggregate_output_tokens,
            ..
        } => {
            println!(
                "    [Aggregate tokens: {} input, {} output]",
                aggregate_input_tokens, aggregate_output_tokens
            );
        }
    }
}
