//! Example: Tool calling with OpenAI API using the #[tool] macro
//!
//! Usage:
//!   OPENAI_API_KEY=your-key cargo run --example openai_tools

use std::collections::HashMap;
use std::env;
use std::io::{self, Write};
use std::sync::Arc;

use llm_rs::llm::{ChatOptions, LLMEvent, LLMMessage, OpenAI, StopReason, LLM};
use llm_rs::tool;
use llm_rs::tool::{CancellationToken, Tool, ToolContext};
use tokio_stream::StreamExt;

// Tool definitions using the #[tool] macro
// The macro generates:
// - A params struct (e.g., GetWeatherParams)
// - A tool constructor function (e.g., get_weather_tool())

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

/// Get the current time
#[tool]
fn get_current_time() -> impl tokio_stream::Stream<Item = Result<String, String>> {
    let now = chrono::Local::now();
    tokio_stream::once(Ok(now.format("%Y-%m-%d %H:%M:%S").to_string()))
}

#[tokio::main]
async fn main() {
    let api_key = env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let model = "gpt-4o-mini";

    // Create tools using the generated constructor functions
    let weather_tool = Arc::new(get_weather_tool());
    let time_tool = Arc::new(get_current_time_tool());

    // Tools list for registration
    let tools_list = vec![Arc::clone(&weather_tool), Arc::clone(&time_tool)];

    // HashMap for tool lookup during execution
    let mut tools: HashMap<String, Arc<Tool>> = HashMap::new();
    tools.insert("get_weather".to_string(), weather_tool);
    tools.insert("get_current_time".to_string(), time_tool);

    let mut client = OpenAI::new(&api_key);

    // Register tools with the LLM for caching
    client.register_tools(tools_list);

    let messages = vec![
        LLMMessage::System("You are a helpful assistant. Use tools when needed.".to_string()),
        LLMMessage::User("What's the weather in Tokyo?".to_string()),
    ];

    println!("User: What's the weather in Tokyo?");
    print!("Assistant: ");
    io::stdout().flush().unwrap();

    let mut stream = client.chat(&model, &messages, &ChatOptions::default());
    let mut pending_tool_calls = Vec::new();

    while let Some(event) = stream.next().await {
        match event {
            LLMEvent::MessageStart { .. } => {}
            LLMEvent::TextDelta(text) => {
                print!("{}", text);
                io::stdout().flush().unwrap();
            }
            LLMEvent::ThinkingDelta(text) => {
                print!("[thinking: {}]", text);
                io::stdout().flush().unwrap();
            }
            LLMEvent::ToolCall(tool_call) => {
                pending_tool_calls.push(tool_call);
            }
            LLMEvent::MessageEnd { stop_reason, .. } => {
                println!();

                if stop_reason == StopReason::ToolUse && !pending_tool_calls.is_empty() {
                    println!();
                    println!("--- Tool Calls Requested ---");

                    for tool_call in &pending_tool_calls {
                        println!("Tool: {}", tool_call.name);
                        println!("Args: {}", tool_call.arguments);

                        // Find and execute the tool
                        let result = if let Some(tool) = tools.get(&tool_call.name) {
                            let ctx = ToolContext { cancel_token: CancellationToken::new() };
                            let mut result_stream = tool.execute(ctx, tool_call.arguments.clone());
                            let mut result = String::new();
                            while let Some(chunk) = result_stream.next().await {
                                result.push_str(&chunk);
                            }
                            result
                        } else {
                            format!("Unknown tool: {}", tool_call.name)
                        };

                        println!("Result: {}", result);
                    }

                    println!();
                    println!("--- Conversation ended after tool execution ---");
                } else {
                    println!();
                    println!("--- Done (no tool calls) ---");
                }
            }
            LLMEvent::Error(err) => {
                eprintln!("\nError: {}", err);
            }
        }
    }
}
