use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_stream::stream;
use aws_config::{BehaviorVersion, Region};
use aws_sdk_bedrockruntime::Client;
use aws_sdk_bedrockruntime::config::Config as BedrockConfig;
use aws_sdk_bedrockruntime::types::{
    CachePointBlock, CachePointType, ContentBlock, ContentBlockDelta, ContentBlockStart,
    ConversationRole, ConverseStreamOutput, DocumentBlock, DocumentFormat, DocumentSource,
    ImageBlock, ImageFormat, ImageSource, InferenceConfiguration, Message, ReasoningContentBlock,
    ReasoningContentBlockDelta, ReasoningTextBlock, StopReason as AwsStopReason,
    SystemContentBlock, ToolConfiguration, ToolInputSchema, ToolResultBlock,
    ToolResultContentBlock, ToolSpecification, ToolUseBlock,
};
use aws_smithy_types::{Blob, Document, Number};
use base64::Engine;
use serde_json::{Value, json};
use tokio_stream::Stream;

use super::{
    ChatOptions, LLM, LLMEvent, LLMMessage, ModelInfo, ReasoningEffort, StopReason, ToolCall,
};
use crate::media::ContentPart;
use crate::tool::{self, Tool};

const TOOL_PREFIX: &str = "mcp_";
const DEFAULT_OUTPUT_TOKENS: u32 = 8192;
pub(super) const MAX_CACHE_POINTS_PER_REQUEST: usize = 4;

pub struct Bedrock {
    client: Client,
    region: String,
    model_id: String,
    endpoint_url: Option<String>,
    cached_tool_defs: Option<ToolConfiguration>,
    pub media_dir: Option<PathBuf>,
}

impl Bedrock {
    pub async fn new(region: &str, model_id: &str, endpoint_url: Option<String>) -> Result<Self> {
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if !region.is_empty() {
            loader = loader.region(Region::new(region.to_string()));
        }
        let shared_config = loader.load().await;
        let resolved_region = shared_config
            .region()
            .map(|region| region.as_ref().to_string())
            .unwrap_or_else(|| "us-east-1".to_string());

        let mut config_builder = BedrockConfig::from(&shared_config)
            .to_builder()
            .region(Region::new(resolved_region.clone()));
        if let Some(endpoint) = endpoint_url.as_deref()
            && !endpoint.is_empty()
        {
            config_builder = config_builder.endpoint_url(endpoint);
        }
        let client = Client::from_conf(config_builder.build());

        Ok(Self {
            client,
            region: resolved_region,
            model_id: model_id.to_string(),
            endpoint_url,
            cached_tool_defs: None,
            media_dir: None,
        })
    }
}

pub(super) struct ConvertedMessages {
    pub(super) system: Option<Vec<SystemContentBlock>>,
    pub(super) messages: Vec<Message>,
}

struct ToolBlockAccumulator {
    id: String,
    name: String,
    input_json: String,
}

struct ThinkingBlockAccumulator {
    text: String,
    signature: String,
    redacted_content: Vec<String>,
}

fn strip_tool_prefix(name: &str) -> String {
    name.strip_prefix(TOOL_PREFIX).unwrap_or(name).to_string()
}

fn cache_point() -> Result<CachePointBlock> {
    CachePointBlock::builder()
        .r#type(CachePointType::Default)
        .build()
        .context("failed to build Bedrock cache point")
}

fn value_to_document(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(v) => Document::Bool(*v),
        Value::Number(n) => {
            if let Some(v) = n.as_u64() {
                Document::Number(Number::PosInt(v))
            } else if let Some(v) = n.as_i64() {
                Document::Number(Number::NegInt(v))
            } else if let Some(v) = n.as_f64() {
                Document::Number(Number::Float(v))
            } else {
                Document::Null
            }
        }
        Value::String(v) => Document::String(v.clone()),
        Value::Array(values) => Document::Array(values.iter().map(value_to_document).collect()),
        Value::Object(map) => Document::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), value_to_document(value)))
                .collect(),
        ),
    }
}

fn thinking_budget(options: &ChatOptions) -> Option<u32> {
    if let Some(budget) = options.reasoning_budget {
        Some(budget)
    } else {
        options
            .reasoning_effort
            .as_ref()
            .map(|effort| match effort {
                ReasoningEffort::Minimal => 4000,
                ReasoningEffort::Low => 8000,
                ReasoningEffort::Medium => 16000,
                ReasoningEffort::High => 24000,
                ReasoningEffort::XHigh => 31999,
            })
    }
}

pub(super) fn build_thinking_document(budget: u32) -> Document {
    Document::Object(HashMap::from([(
        "thinking".to_string(),
        Document::Object(HashMap::from([
            ("type".to_string(), Document::String("enabled".to_string())),
            (
                "budget_tokens".to_string(),
                Document::Number(Number::PosInt(u64::from(budget))),
            ),
        ])),
    )]))
}

pub(super) fn build_tool_config(tools: &[Arc<Tool>]) -> Result<Option<ToolConfiguration>> {
    if tools.is_empty() {
        return Ok(None);
    }

    let tool_specs = tools
        .iter()
        .map(|t| {
            let schema = value_to_document(&tool::normalize_schema(&t.param_schema));
            let spec = ToolSpecification::builder()
                .name(format!("{}{}", TOOL_PREFIX, t.name))
                .description(t.description.clone())
                .input_schema(ToolInputSchema::Json(schema))
                .build()
                .context("failed to build Bedrock tool specification")?;
            Ok(aws_sdk_bedrockruntime::types::Tool::ToolSpec(spec))
        })
        .collect::<Result<Vec<_>>>()?;

    ToolConfiguration::builder()
        .set_tools(Some(tool_specs))
        .build()
        .map(Some)
        .context("failed to build Bedrock tool configuration")
}

fn image_format(media_type: &str) -> Result<ImageFormat> {
    match media_type {
        "image/png" => Ok(ImageFormat::Png),
        "image/jpeg" => Ok(ImageFormat::Jpeg),
        "image/gif" => Ok(ImageFormat::Gif),
        "image/webp" => Ok(ImageFormat::Webp),
        other => bail!("unsupported Bedrock image media type: {other}"),
    }
}

fn document_format(media_type: &str) -> Result<DocumentFormat> {
    match media_type {
        "application/pdf" => Ok(DocumentFormat::Pdf),
        "text/plain" => Ok(DocumentFormat::Txt),
        "text/markdown" => Ok(DocumentFormat::Md),
        "text/html" => Ok(DocumentFormat::Html),
        "text/csv" => Ok(DocumentFormat::Csv),
        other => bail!("unsupported Bedrock document media type: {other}"),
    }
}

fn media_content_block(
    media: &crate::media::MediaData,
    media_dir: &Option<PathBuf>,
) -> Result<ContentBlock> {
    let media_dir = media_dir
        .as_ref()
        .context("Media present in message but no media_dir configured")?;
    let data = media.get_data(media_dir)?.to_vec();
    let media_type = media.media_type();

    if media_type == "application/pdf" || media_type.starts_with("text/") {
        let block = DocumentBlock::builder()
            .format(document_format(media_type)?)
            .name("document")
            .source(DocumentSource::Bytes(Blob::new(data)))
            .build()
            .context("failed to build Bedrock document block")?;
        Ok(ContentBlock::Document(block))
    } else {
        let block = ImageBlock::builder()
            .format(image_format(media_type)?)
            .source(ImageSource::Bytes(Blob::new(data)))
            .build()
            .context("failed to build Bedrock image block")?;
        Ok(ContentBlock::Image(block))
    }
}

fn convert_raw_content(raw: &Value) -> Result<Option<Vec<ContentBlock>>> {
    let Some(blocks) = raw.get("content").and_then(Value::as_array) else {
        return Ok(None);
    };

    let mut content = Vec::new();
    for block in blocks {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match block_type {
            "text" => {
                if let Some(text) = block.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    content.push(ContentBlock::Text(text.to_string()));
                }
            }
            "tool_use" => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let raw_name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let name = if raw_name.starts_with(TOOL_PREFIX) {
                    raw_name.to_string()
                } else {
                    format!("{TOOL_PREFIX}{raw_name}")
                };
                let input = block
                    .get("input")
                    .map(value_to_document)
                    .unwrap_or(Document::Null);
                let tool_use = ToolUseBlock::builder()
                    .tool_use_id(id)
                    .name(name)
                    .input(input)
                    .build()
                    .context("failed to build raw Bedrock tool use block")?;
                content.push(ContentBlock::ToolUse(tool_use));
            }
            "thinking" => {
                if let Some(redacted_blocks) =
                    block.get("redacted_content").and_then(Value::as_array)
                {
                    for redacted in redacted_blocks {
                        if let Some(encoded) = redacted.as_str()
                            && !encoded.is_empty()
                        {
                            let bytes = base64::engine::general_purpose::STANDARD
                                .decode(encoded)
                                .context(
                                "failed to decode Bedrock redacted reasoning content",
                            )?;
                            content.push(ContentBlock::ReasoningContent(
                                ReasoningContentBlock::RedactedContent(Blob::new(bytes)),
                            ));
                        }
                    }
                }
                let text = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !text.is_empty() {
                    let mut builder = ReasoningTextBlock::builder().text(text);
                    if let Some(signature) = block.get("signature").and_then(Value::as_str)
                        && !signature.is_empty()
                    {
                        builder = builder.signature(signature.to_string());
                    }
                    content.push(ContentBlock::ReasoningContent(
                        ReasoningContentBlock::ReasoningText(
                            builder
                                .build()
                                .context("failed to build raw Bedrock reasoning block")?,
                        ),
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(Some(content))
}

fn assistant_content_blocks(content: &str, tool_calls: &[ToolCall]) -> Result<Vec<ContentBlock>> {
    let mut blocks = Vec::new();
    if !content.is_empty() {
        blocks.push(ContentBlock::Text(content.to_string()));
    }
    for tool_call in tool_calls {
        let input_value: Value = serde_json::from_str(&tool_call.arguments).unwrap_or(Value::Null);
        let tool_use = ToolUseBlock::builder()
            .tool_use_id(tool_call.id.clone())
            .name(format!("{}{}", TOOL_PREFIX, tool_call.name))
            .input(value_to_document(&input_value))
            .build()
            .context("failed to build Bedrock assistant tool use block")?;
        blocks.push(ContentBlock::ToolUse(tool_use));
    }
    if blocks.is_empty() {
        blocks.push(ContentBlock::Text(String::new()));
    }
    Ok(blocks)
}

fn convert_tool_result_content(
    tool_call_id: &str,
    content: &[ContentPart],
    media_dir: &Option<PathBuf>,
) -> Result<ContentBlock> {
    let mut result_parts = Vec::new();
    let text = crate::media::join_text_parts(content);
    if !text.is_empty() || crate::llm::is_all_text(content) {
        result_parts.push(ToolResultContentBlock::Text(text));
    }

    for part in content {
        match part {
            ContentPart::Text(_) => {}
            ContentPart::Media(media) => match media_content_block(media, media_dir)? {
                ContentBlock::Image(block) => {
                    result_parts.push(ToolResultContentBlock::Image(block))
                }
                ContentBlock::Document(block) => {
                    result_parts.push(ToolResultContentBlock::Document(block))
                }
                _ => {}
            },
        }
    }

    if result_parts.is_empty() {
        result_parts.push(ToolResultContentBlock::Text(String::new()));
    }

    let block = ToolResultBlock::builder()
        .tool_use_id(tool_call_id.to_string())
        .set_content(Some(result_parts))
        .build()
        .context("failed to build Bedrock tool result block")?;
    Ok(ContentBlock::ToolResult(block))
}

fn push_user_message(
    messages: &mut Vec<Message>,
    content: Vec<ContentBlock>,
    error_context: &str,
) -> Result<()> {
    messages.push(
        Message::builder()
            .role(ConversationRole::User)
            .set_content(Some(content))
            .build()
            .context(error_context.to_string())?,
    );
    Ok(())
}

fn flush_pending_tool_results(
    messages: &mut Vec<Message>,
    pending_tool_results: &mut Vec<ContentBlock>,
) -> Result<()> {
    if pending_tool_results.is_empty() {
        return Ok(());
    }
    let content = std::mem::take(pending_tool_results);
    push_user_message(
        messages,
        content,
        "failed to build Bedrock tool result message",
    )
}

pub(super) fn convert_to_sdk_messages(
    msgs: &[LLMMessage],
    media_dir: &Option<PathBuf>,
) -> Result<ConvertedMessages> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    let mut pending_tool_results = Vec::new();

    for msg in msgs {
        match msg {
            LLMMessage::System(content) => {
                flush_pending_tool_results(&mut messages, &mut pending_tool_results)?;
                if !content.is_empty() {
                    system.push(SystemContentBlock::Text(content.clone()));
                }
            }
            LLMMessage::User(parts) => {
                flush_pending_tool_results(&mut messages, &mut pending_tool_results)?;
                let mut content = Vec::new();
                let mut has_document = false;
                let mut has_text = false;

                for part in parts {
                    match part {
                        ContentPart::Text(text) => {
                            if !text.is_empty() {
                                has_text = true;
                                content.push(ContentBlock::Text(text.clone()));
                            }
                        }
                        ContentPart::Media(media) => {
                            let block = media_content_block(media, media_dir)?;
                            has_document |= matches!(block, ContentBlock::Document(_));
                            content.push(block);
                        }
                    }
                }

                if has_document && !has_text {
                    content.insert(0, ContentBlock::Text("Document attached.".to_string()));
                }
                if content.is_empty() {
                    content.push(ContentBlock::Text(String::new()));
                }

                push_user_message(
                    &mut messages,
                    content,
                    "failed to build Bedrock user message",
                )?;
            }
            LLMMessage::Assistant {
                content,
                tool_calls,
                raw,
            } => {
                flush_pending_tool_results(&mut messages, &mut pending_tool_results)?;
                let blocks = if let Some(raw) = raw {
                    convert_raw_content(raw)?
                        .unwrap_or(assistant_content_blocks(content, tool_calls)?)
                } else {
                    assistant_content_blocks(content, tool_calls)?
                };
                messages.push(
                    Message::builder()
                        .role(ConversationRole::Assistant)
                        .set_content(Some(blocks))
                        .build()
                        .context("failed to build Bedrock assistant message")?,
                );
            }
            LLMMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                pending_tool_results.push(convert_tool_result_content(
                    tool_call_id,
                    content,
                    media_dir,
                )?);
            }
        }
    }

    flush_pending_tool_results(&mut messages, &mut pending_tool_results)?;

    let mut cache_points_used = 0;
    if !system.is_empty() {
        system.push(SystemContentBlock::CachePoint(cache_point()?));
        cache_points_used += 1;
    }

    for message in messages.iter_mut().rev() {
        if cache_points_used >= MAX_CACHE_POINTS_PER_REQUEST {
            break;
        }
        if matches!(&message.role, ConversationRole::User) {
            message
                .content
                .push(ContentBlock::CachePoint(cache_point()?));
            cache_points_used += 1;
        }
    }

    Ok(ConvertedMessages {
        system: (!system.is_empty()).then_some(system),
        messages,
    })
}

fn map_stop_reason(reason: &AwsStopReason) -> Result<StopReason, String> {
    match reason {
        AwsStopReason::EndTurn | AwsStopReason::StopSequence => Ok(StopReason::EndTurn),
        AwsStopReason::ToolUse => Ok(StopReason::ToolUse),
        AwsStopReason::MaxTokens => Ok(StopReason::MaxTokens),
        other => Err(format!(
            "Bedrock stopped generation with abnormal stop reason: {}",
            other.as_str()
        )),
    }
}

fn raw_from_content(content: Vec<Value>) -> Value {
    json!({
        "role": "assistant",
        "content": content,
    })
}

pub(super) struct BedrockStreamState {
    emitted_start: bool,
    input_tokens: i32,
    output_tokens: i32,
    cache_creation_input_tokens: i32,
    cache_read_input_tokens: i32,
    reasoning_tokens: i32,
    stop_reason: StopReason,
    text_blocks: HashMap<usize, String>,
    tool_blocks: HashMap<usize, ToolBlockAccumulator>,
    thinking_blocks: HashMap<usize, ThinkingBlockAccumulator>,
    accumulated_content: Vec<Value>,
}

impl BedrockStreamState {
    pub(super) fn new() -> Self {
        Self {
            emitted_start: false,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            reasoning_tokens: 0,
            stop_reason: StopReason::EndTurn,
            text_blocks: HashMap::new(),
            tool_blocks: HashMap::new(),
            thinking_blocks: HashMap::new(),
            accumulated_content: Vec::new(),
        }
    }

    pub(super) fn handle_event(&mut self, event: ConverseStreamOutput) -> Vec<LLMEvent> {
        let mut events = Vec::new();
        match event {
            ConverseStreamOutput::MessageStart(_) => {
                if !self.emitted_start {
                    events.push(LLMEvent::MessageStart {
                        input_tokens: self.input_tokens,
                    });
                    self.emitted_start = true;
                }
            }
            ConverseStreamOutput::ContentBlockStart(event) => {
                let index = event.content_block_index.max(0) as usize;
                if let Some(ContentBlockStart::ToolUse(tool_use)) = event.start {
                    let raw_name = tool_use.name;
                    let name = strip_tool_prefix(&raw_name);
                    events.push(LLMEvent::ToolCallStart {
                        index,
                        id: tool_use.tool_use_id.clone(),
                        name,
                    });
                    self.tool_blocks.insert(
                        index,
                        ToolBlockAccumulator {
                            id: tool_use.tool_use_id,
                            name: raw_name,
                            input_json: String::new(),
                        },
                    );
                }
            }
            ConverseStreamOutput::ContentBlockDelta(event) => {
                let index = event.content_block_index.max(0) as usize;
                if let Some(delta) = event.delta {
                    match delta {
                        ContentBlockDelta::Text(text) => {
                            if !text.is_empty() {
                                self.text_blocks.entry(index).or_default().push_str(&text);
                                events.push(LLMEvent::TextDelta(text));
                            }
                        }
                        ContentBlockDelta::ToolUse(delta) => {
                            let partial = delta.input;
                            if let Some(acc) = self.tool_blocks.get_mut(&index) {
                                acc.input_json.push_str(&partial);
                            }
                            events.push(LLMEvent::ToolCallDelta {
                                index,
                                partial_json: partial,
                            });
                        }
                        ContentBlockDelta::ReasoningContent(delta) => match delta {
                            ReasoningContentBlockDelta::Text(text) => {
                                if !text.is_empty() {
                                    self.thinking_blocks
                                        .entry(index)
                                        .or_insert_with(|| ThinkingBlockAccumulator {
                                            text: String::new(),
                                            signature: String::new(),
                                            redacted_content: Vec::new(),
                                        })
                                        .text
                                        .push_str(&text);
                                    events.push(LLMEvent::ThinkingDelta(text));
                                }
                            }
                            ReasoningContentBlockDelta::Signature(signature) => {
                                self.thinking_blocks
                                    .entry(index)
                                    .or_insert_with(|| ThinkingBlockAccumulator {
                                        text: String::new(),
                                        signature: String::new(),
                                        redacted_content: Vec::new(),
                                    })
                                    .signature
                                    .push_str(&signature);
                            }
                            ReasoningContentBlockDelta::RedactedContent(blob) => {
                                let encoded =
                                    base64::engine::general_purpose::STANDARD.encode(blob.as_ref());
                                self.thinking_blocks
                                    .entry(index)
                                    .or_insert_with(|| ThinkingBlockAccumulator {
                                        text: String::new(),
                                        signature: String::new(),
                                        redacted_content: Vec::new(),
                                    })
                                    .redacted_content
                                    .push(encoded);
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
            }
            ConverseStreamOutput::ContentBlockStop(event) => {
                let index = event.content_block_index.max(0) as usize;
                if let Some(acc) = self.tool_blocks.remove(&index) {
                    let input = serde_json::from_str::<Value>(&acc.input_json)
                        .unwrap_or_else(|_| json!({}));
                    self.accumulated_content.push(json!({
                        "type": "tool_use",
                        "id": acc.id,
                        "name": acc.name,
                        "input": input,
                    }));
                    let raw_name = self
                        .accumulated_content
                        .last()
                        .and_then(|value| value.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let id = self
                        .accumulated_content
                        .last()
                        .and_then(|value| value.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let arguments = self
                        .accumulated_content
                        .last()
                        .and_then(|value| value.get("input"))
                        .map(Value::to_string)
                        .unwrap_or_else(|| "{}".to_string());
                    events.push(LLMEvent::ToolCall(ToolCall {
                        id,
                        name: strip_tool_prefix(&raw_name),
                        arguments,
                    }));
                } else if let Some(acc) = self.thinking_blocks.remove(&index) {
                    if acc.redacted_content.is_empty() {
                        self.accumulated_content.push(json!({
                            "type": "thinking",
                            "thinking": acc.text,
                            "signature": acc.signature,
                        }));
                    } else {
                        self.accumulated_content.push(json!({
                            "type": "thinking",
                            "thinking": acc.text,
                            "signature": acc.signature,
                            "redacted_content": acc.redacted_content,
                        }));
                    }
                } else if let Some(text) = self.text_blocks.remove(&index)
                    && !text.is_empty()
                {
                    self.accumulated_content.push(json!({
                        "type": "text",
                        "text": text,
                    }));
                }
            }
            ConverseStreamOutput::MessageStop(event) => match map_stop_reason(&event.stop_reason) {
                Ok(stop_reason) => self.stop_reason = stop_reason,
                Err(error) => events.push(LLMEvent::Error(error)),
            },
            ConverseStreamOutput::Metadata(event) => {
                if let Some(usage) = event.usage {
                    self.input_tokens = usage.input_tokens;
                    self.output_tokens = usage.output_tokens;
                    self.cache_creation_input_tokens = usage.cache_write_input_tokens.unwrap_or(0);
                    self.cache_read_input_tokens = usage.cache_read_input_tokens.unwrap_or(0);
                }
                events.push(self.finish_event());
            }
            _ => {}
        }
        events
    }

    pub(super) fn finish_event(&mut self) -> LLMEvent {
        let mut remaining_text: Vec<_> = self.text_blocks.drain().collect();
        remaining_text.sort_by_key(|(idx, _)| *idx);
        for (_, text) in remaining_text {
            if !text.is_empty() {
                self.accumulated_content
                    .push(json!({"type": "text", "text": text}));
            }
        }
        LLMEvent::MessageEnd {
            stop_reason: self.stop_reason.clone(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            raw: Some(raw_from_content(std::mem::take(
                &mut self.accumulated_content,
            ))),
        }
    }
}

impl LLM for Bedrock {
    fn register_tools(&mut self, tools: Vec<Arc<Tool>>) {
        self.cached_tool_defs = match build_tool_config(&tools) {
            Ok(config) => config,
            Err(e) => {
                tracing::error!(error = %e, "failed to build Bedrock tool configuration");
                None
            }
        };
    }

    fn chat(
        &self,
        model: &str,
        msgs: &[LLMMessage],
        options: &ChatOptions,
    ) -> Pin<Box<dyn Stream<Item = LLMEvent> + Send>> {
        let client = self.client.clone();
        let model = if model.is_empty() {
            self.model_id.clone()
        } else {
            model.to_string()
        };
        let tool_config = self.cached_tool_defs.clone();
        let media_dir = self.media_dir.clone();
        let converted = match convert_to_sdk_messages(msgs, &media_dir) {
            Ok(converted) => converted,
            Err(e) => {
                return Box::pin(stream! {
                    yield LLMEvent::Error(format!("Failed to convert messages for Bedrock: {e:#}"));
                });
            }
        };

        let budget = thinking_budget(options);
        let max_tokens = match (budget, options.max_tokens) {
            (_, Some(user_max)) => user_max,
            (Some(budget), None) => budget + DEFAULT_OUTPUT_TOKENS,
            (None, None) => DEFAULT_OUTPUT_TOKENS,
        };
        let additional_fields = budget.map(build_thinking_document);
        let inference_config = InferenceConfiguration::builder()
            .max_tokens(i32::try_from(max_tokens).unwrap_or(i32::MAX))
            .build();

        Box::pin(stream! {
            let mut request = client
                .converse_stream()
                .model_id(model)
                .set_messages(Some(converted.messages))
                .inference_config(inference_config);

            if let Some(system) = converted.system {
                request = request.set_system(Some(system));
            }
            if let Some(tool_config) = tool_config {
                request = request.tool_config(tool_config);
            }
            if let Some(additional_fields) = additional_fields {
                request = request.additional_model_request_fields(additional_fields);
            }

            let response = match request.send().await {
                Ok(response) => response,
                Err(e) => {
                    yield LLMEvent::Error(format!("Bedrock ConverseStream request failed: {e}"));
                    return;
                }
            };

            let mut stream = response.stream;
            let mut state = BedrockStreamState::new();

            while let Some(event) = match stream.recv().await {
                Ok(event) => event,
                Err(e) => {
                    yield LLMEvent::Error(format!("Bedrock stream error: {e}"));
                    return;
                }
            } {
                let events = state.handle_event(event);
                let should_return = events.iter().any(|event| {
                    matches!(event, LLMEvent::MessageEnd { .. } | LLMEvent::Error(_))
                });
                for event in events {
                    yield event;
                }
                if should_return {
                    return;
                }
            }

            yield state.finish_event();
        })
    }

    fn clone_box(&self) -> Box<dyn LLM> {
        Box::new(Bedrock {
            client: self.client.clone(),
            region: self.region.clone(),
            model_id: self.model_id.clone(),
            endpoint_url: self.endpoint_url.clone(),
            cached_tool_defs: None,
            media_dir: self.media_dir.clone(),
        })
    }

    fn set_media_dir(&mut self, dir: Option<PathBuf>) {
        self.media_dir = dir;
    }

    fn available_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "us.anthropic.claude-opus-4-6-v1".into(),
                description: "Most capable Claude model via Bedrock".into(),
            },
            ModelInfo {
                id: "us.anthropic.claude-sonnet-4-6-v1".into(),
                description: "Balanced speed and capability via Bedrock".into(),
            },
            ModelInfo {
                id: "us.anthropic.claude-haiku-4-5-v1".into(),
                description: "Fast and cost-effective Claude model via Bedrock".into(),
            },
        ]
    }
}
