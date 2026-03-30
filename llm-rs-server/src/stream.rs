//! Convert LLMEvent streams into OpenAI SSE chunks or accumulated responses.

use std::convert::Infallible;
use std::pin::Pin;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::response::sse::{Event, Sse};
use llm_rs::llm::LLMEvent;
use tokio_stream::{Stream, StreamExt};

use crate::convert::{stop_reason_to_finish_reason, tool_call_to_message};
use crate::error::AppError;
use crate::types::*;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn make_usage(input_tokens: i32, output_tokens: i32, reasoning_tokens: i32) -> UsageResponse {
    UsageResponse {
        prompt_tokens: input_tokens,
        completion_tokens: output_tokens,
        total_tokens: input_tokens + output_tokens,
        completion_tokens_details: if reasoning_tokens > 0 {
            Some(CompletionTokensDetails { reasoning_tokens })
        } else {
            None
        },
    }
}

/// Build an SSE streaming response from an LLMEvent stream.
pub fn streaming_response(
    llm_stream: Pin<Box<dyn Stream<Item = LLMEvent> + Send>>,
    model: String,
    stream_options: Option<StreamOptions>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = now_unix();
    let include_usage = stream_options.as_ref().is_some_and(|o| o.include_usage);

    let stream = async_stream::stream! {
        let mut tool_call_index: u32 = 0;
        tokio::pin!(llm_stream);

        while let Some(event) = llm_stream.next().await {
            match event {
                LLMEvent::MessageStart { .. } => {
                    let chunk = ChatCompletionChunk {
                        id: id.clone(),
                        object: "chat.completion.chunk".into(),
                        created,
                        model: model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                role: Some("assistant".into()),
                                ..Default::default()
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    yield Ok(Event::default().data(serde_json::to_string(&chunk).expect("ChatCompletionChunk is always serializable")));
                }
                LLMEvent::TextDelta(text) => {
                    let chunk = ChatCompletionChunk {
                        id: id.clone(),
                        object: "chat.completion.chunk".into(),
                        created,
                        model: model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                content: Some(text),
                                ..Default::default()
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    yield Ok(Event::default().data(serde_json::to_string(&chunk).expect("ChatCompletionChunk is always serializable")));
                }
                LLMEvent::ThinkingDelta(text) => {
                    let chunk = ChatCompletionChunk {
                        id: id.clone(),
                        object: "chat.completion.chunk".into(),
                        created,
                        model: model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                reasoning_content: Some(text),
                                ..Default::default()
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    yield Ok(Event::default().data(serde_json::to_string(&chunk).expect("ChatCompletionChunk is always serializable")));
                }
                LLMEvent::ToolCall(tc) => {
                    let idx = tool_call_index;
                    tool_call_index += 1;
                    let chunk = ChatCompletionChunk {
                        id: id.clone(),
                        object: "chat.completion.chunk".into(),
                        created,
                        model: model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta {
                                tool_calls: Some(vec![ChunkToolCallDelta {
                                    index: idx,
                                    id: Some(tc.id),
                                    call_type: Some("function".into()),
                                    function: ChunkToolCallFunction {
                                        name: Some(tc.name),
                                        arguments: Some(tc.arguments),
                                    },
                                }]),
                                ..Default::default()
                            },
                            finish_reason: None,
                        }],
                        usage: None,
                    };
                    yield Ok(Event::default().data(serde_json::to_string(&chunk).expect("ChatCompletionChunk is always serializable")));
                }
                LLMEvent::MessageEnd { stop_reason, input_tokens, output_tokens, reasoning_tokens, .. } => {
                    // Chunk with finish_reason
                    let chunk = ChatCompletionChunk {
                        id: id.clone(),
                        object: "chat.completion.chunk".into(),
                        created,
                        model: model.clone(),
                        choices: vec![ChunkChoice {
                            index: 0,
                            delta: ChunkDelta::default(),
                            finish_reason: Some(
                                stop_reason_to_finish_reason(&stop_reason).to_string(),
                            ),
                        }],
                        usage: None,
                    };
                    yield Ok(Event::default().data(serde_json::to_string(&chunk).expect("ChatCompletionChunk is always serializable")));

                    // Separate usage chunk (OpenAI sends usage with empty choices)
                    if include_usage {
                        let usage_chunk = ChatCompletionChunk {
                            id: id.clone(),
                            object: "chat.completion.chunk".into(),
                            created,
                            model: model.clone(),
                            choices: vec![],
                            usage: Some(make_usage(input_tokens, output_tokens, reasoning_tokens)),
                        };
                        yield Ok(Event::default().data(serde_json::to_string(&usage_chunk).expect("ChatCompletionChunk is always serializable")));
                    }
                }
                LLMEvent::Error(e) => {
                    let err = serde_json::json!({
                        "error": { "message": e, "type": "server_error" }
                    });
                    yield Ok(Event::default().data(err.to_string()));
                }
                LLMEvent::ToolCallStart { .. } | LLMEvent::ToolCallDelta { .. } => {
                    // Not forwarded in the Chat Completions SSE path
                }
            }
        }

        // [DONE] sentinel
        yield Ok(Event::default().data("[DONE]"));
    };

    Sse::new(stream)
}

/// Accumulate an LLMEvent stream into a single non-streaming response.
pub async fn non_streaming_response(
    llm_stream: Pin<Box<dyn Stream<Item = LLMEvent> + Send>>,
    model: String,
) -> Result<ChatCompletionResponse, AppError> {
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    let created = now_unix();

    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: Vec<MessageToolCall> = Vec::new();
    let mut finish_reason = "stop".to_string();
    let mut input_tokens = 0i32;
    let mut output_tokens = 0i32;
    let mut reasoning_tokens = 0i32;

    tokio::pin!(llm_stream);

    while let Some(event) = llm_stream.next().await {
        match event {
            LLMEvent::MessageStart { .. } => {}
            LLMEvent::TextDelta(text) => content.push_str(&text),
            LLMEvent::ThinkingDelta(text) => reasoning_content.push_str(&text),
            LLMEvent::ToolCall(tc) => {
                tool_calls.push(tool_call_to_message(&tc));
            }
            LLMEvent::MessageEnd {
                stop_reason,
                input_tokens: it,
                output_tokens: ot,
                reasoning_tokens: rt,
                ..
            } => {
                finish_reason = stop_reason_to_finish_reason(&stop_reason).to_string();
                input_tokens = it;
                output_tokens = ot;
                reasoning_tokens = rt;
            }
            LLMEvent::Error(e) => {
                return Err(AppError::LLMError(e));
            }
            LLMEvent::ToolCallStart { .. } | LLMEvent::ToolCallDelta { .. } => {
                // Not used in non-streaming path
            }
        }
    }

    Ok(ChatCompletionResponse {
        id,
        object: "chat.completion".into(),
        created,
        model,
        choices: vec![ResponseChoice {
            index: 0,
            message: ResponseMessage {
                role: "assistant".into(),
                content: if content.is_empty() {
                    None
                } else {
                    Some(content)
                },
                reasoning_content: if reasoning_content.is_empty() {
                    None
                } else {
                    Some(reasoning_content)
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
            },
            finish_reason,
        }],
        usage: Some(make_usage(input_tokens, output_tokens, reasoning_tokens)),
    })
}
