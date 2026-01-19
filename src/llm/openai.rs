//! OpenAI-compatible LLM implementation (works with OpenRouter).

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use super::{LLMEvent, LLMRole, StopReason, ToolCall, LLM};
use crate::tool::ToolSchema;

/// OpenAI-compatible LLM client.
///
/// Works with OpenAI API, OpenRouter, and other compatible providers.
pub struct OpenAI {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAI {
    /// Create a new OpenAI client.
    ///
    /// # Arguments
    /// - `api_key`: API key for authentication
    /// - `base_url`: Base URL for the API (e.g., "https://api.openai.com/v1" or "https://openrouter.ai/api/v1")
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// Create a new OpenRouter client.
    pub fn openrouter(api_key: impl Into<String>) -> Self {
        Self::new(api_key, "https://openrouter.ai/api/v1")
    }
}

// ============================================================================
// Request types
// ============================================================================

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ToolDefinition {
    #[serde(rename = "type")]
    tool_type: &'static str,
    function: FunctionDefinition,
}

#[derive(Serialize)]
struct FunctionDefinition {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ============================================================================
// Response types (streaming)
// ============================================================================

#[derive(Deserialize, Debug)]
struct ChatCompletionChunk {
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Debug)]
struct ChunkChoice {
    delta: Delta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallDelta>>,
}

#[derive(Deserialize, Debug)]
struct ToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionDelta>,
}

#[derive(Deserialize, Debug, Default)]
struct FunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct Usage {
    #[serde(default)]
    prompt_tokens: i32,
    #[serde(default)]
    completion_tokens: i32,
}

// ============================================================================
// Implementation
// ============================================================================

impl LLM for OpenAI {
    fn chat(
        &self,
        model: &str,
        tools: &[Arc<ToolSchema>],
        msgs: &[(LLMRole, String)],
    ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>> {
        let client = self.client.clone();
        let api_key = self.api_key.clone();
        let base_url = self.base_url.clone();
        let model = model.to_string();

        // Convert messages
        let messages: Vec<ChatMessage> = msgs
            .iter()
            .map(|(role, content)| ChatMessage {
                role: match role {
                    LLMRole::System => "system",
                    LLMRole::User => "user",
                    LLMRole::Assistant => "assistant",
                    LLMRole::Tool => "tool",
                },
                content: content.clone(),
            })
            .collect();

        // Convert tools
        let tool_defs: Option<Vec<ToolDefinition>> = if tools.is_empty() {
            None
        } else {
            Some(
                tools
                    .iter()
                    .map(|t| ToolDefinition {
                        tool_type: "function",
                        function: FunctionDefinition {
                            name: t.name.clone(),
                            description: t.description.clone(),
                            parameters: serde_json::to_value(&t.parameters)
                                .unwrap_or(serde_json::json!({})),
                        },
                    })
                    .collect(),
            )
        };

        Box::pin(stream! {
            let request_body = ChatRequest {
                model: &model,
                messages,
                stream: true,
                tools: tool_defs,
                stream_options: Some(StreamOptions { include_usage: true }),
            };

            let url = format!("{}/chat/completions", base_url);
            let response = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send()
                .await;

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    yield LLMEvent::Error(format!("Request failed: {}", e));
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                yield LLMEvent::Error(format!("API error {}: {}", status, body));
                return;
            }

            // Track tool calls being built across chunks
            let mut tool_calls: HashMap<usize, (String, String, String)> = HashMap::new();
            let mut input_tokens = 0i32;
            let mut output_tokens = 0i32;
            let mut emitted_start = false;

            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = byte_stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        yield LLMEvent::Error(format!("Stream error: {}", e));
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete SSE lines
                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    if !line.starts_with("data: ") {
                        continue;
                    }

                    let data = &line[6..];

                    if data == "[DONE]" {
                        // Emit any completed tool calls
                        for (_, (id, name, args)) in tool_calls.drain() {
                            yield LLMEvent::ToolCall(ToolCall {
                                id,
                                name,
                                arguments: args,
                            });
                        }

                        yield LLMEvent::MessageEnd {
                            stop_reason: StopReason::EndTurn,
                            input_tokens,
                            output_tokens,
                        };
                        return;
                    }

                    let chunk: ChatCompletionChunk = match serde_json::from_str(data) {
                        Ok(c) => c,
                        Err(e) => {
                            yield LLMEvent::Error(format!("Parse error: {} - data: {}", e, data));
                            return;
                        }
                    };

                    // Handle usage info (sent with stream_options.include_usage)
                    if let Some(usage) = chunk.usage {
                        input_tokens = usage.prompt_tokens;
                        output_tokens = usage.completion_tokens;
                    }

                    for choice in chunk.choices {
                        // Emit MessageStart on first content
                        if !emitted_start {
                            yield LLMEvent::MessageStart { input_tokens: 0 };
                            emitted_start = true;
                        }

                        // Handle text content
                        if let Some(content) = choice.delta.content {
                            if !content.is_empty() {
                                yield LLMEvent::TextDelta(content);
                            }
                        }

                        // Handle tool calls
                        if let Some(tc_deltas) = choice.delta.tool_calls {
                            for tc_delta in tc_deltas {
                                let entry = tool_calls
                                    .entry(tc_delta.index)
                                    .or_insert_with(|| (String::new(), String::new(), String::new()));

                                if let Some(id) = tc_delta.id {
                                    entry.0 = id;
                                }
                                if let Some(func) = tc_delta.function {
                                    if let Some(name) = func.name {
                                        entry.1 = name;
                                    }
                                    if let Some(args) = func.arguments {
                                        entry.2.push_str(&args);
                                    }
                                }
                            }
                        }

                        // Handle finish reason
                        if let Some(reason) = choice.finish_reason {
                            let stop_reason = match reason.as_str() {
                                "tool_calls" => {
                                    // Emit completed tool calls
                                    for (_, (id, name, args)) in tool_calls.drain() {
                                        yield LLMEvent::ToolCall(ToolCall {
                                            id,
                                            name,
                                            arguments: args,
                                        });
                                    }
                                    StopReason::ToolUse
                                }
                                "length" => StopReason::MaxTokens,
                                _ => StopReason::EndTurn,
                            };

                            yield LLMEvent::MessageEnd {
                                stop_reason,
                                input_tokens,
                                output_tokens,
                            };
                            return;
                        }
                    }
                }
            }

            // Stream ended without [DONE] or finish_reason
            if !tool_calls.is_empty() {
                for (_, (id, name, args)) in tool_calls.drain() {
                    yield LLMEvent::ToolCall(ToolCall {
                        id,
                        name,
                        arguments: args,
                    });
                }
            }

            yield LLMEvent::MessageEnd {
                stop_reason: StopReason::EndTurn,
                input_tokens,
                output_tokens,
            };
        })
    }
}
