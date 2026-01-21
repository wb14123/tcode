//! Example: Tool calling with OpenAI/OpenRouter API using the #[tool] macro
//!
//! Usage:
//!   OPENROUTER_API_KEY=your-key cargo run --example openai_tools

use std::collections::HashMap;
use std::env;
use std::io::{self, Write};
use std::sync::Arc;

use llm_rs::llm::{LLMEvent, LLMRole, OpenAI, StopReason, LLM};
use llm_rs::tool;
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
) -> impl tokio_stream::Stream<Item = String> {
    let result = format!(
        "Weather in {}: 22°C, partly cloudy, humidity 65%",
        city
    );
    tokio_stream::once(result)
}

/// Evaluate a mathematical expression
#[tool]
fn calculate(
    /// Mathematical expression to evaluate (e.g., "2 + 3 * 4")
    expression: String,
) -> impl tokio_stream::Stream<Item = String> {
    // Simple eval for demo (just handles basic cases)
    let expr = expression.trim();
    let result = if let Some((a, b)) = expr.split_once('+') {
        if let (Ok(a), Ok(b)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
            format!("{} = {}", expr, a + b)
        } else {
            format!("Cannot evaluate: {}", expr)
        }
    } else if let Some((a, b)) = expr.split_once('*') {
        if let (Ok(a), Ok(b)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
            format!("{} = {}", expr, a * b)
        } else {
            format!("Cannot evaluate: {}", expr)
        }
    } else {
        format!("Cannot evaluate: {}", expr)
    };
    tokio_stream::once(result)
}

#[tokio::main]
async fn main() {
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

    // Create tools using the generated constructor functions
    let weather_tool = Arc::new(get_weather_tool());
    let calc_tool = Arc::new(calculate_tool());

    // Tools list for registration
    let tools_list = vec![Arc::clone(&weather_tool), Arc::clone(&calc_tool)];

    // HashMap for tool lookup during execution
    let mut tools = HashMap::new();
    tools.insert("get_weather".to_string(), weather_tool);
    tools.insert("calculate".to_string(), calc_tool);

    let mut client = OpenAI::new(&api_key, &base_url);

    // Register tools with the LLM for caching
    client.register_tools(tools_list);

    let messages = vec![
        (
            LLMRole::System,
            "You are a helpful assistant. Use tools when needed.".to_string(),
        ),
        (
            LLMRole::User,
            "What's the weather in Tokyo?".to_string(),
        ),
    ];

    println!("User: What's the weather in Tokyo?");
    print!("Assistant: ");
    io::stdout().flush().unwrap();

    let mut stream = client.chat(&model, &messages);
    let mut pending_tool_calls = Vec::new();

    while let Some(event) = stream.next().await {
        match event {
            LLMEvent::MessageStart { .. } => {}
            LLMEvent::TextDelta(text) => {
                print!("{}", text);
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
                            let mut result_stream = tool.execute(tool_call.arguments.clone());
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
