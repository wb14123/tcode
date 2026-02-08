//! Shared types and helpers for OpenAI-compatible providers.

use std::sync::Arc;

use super::{ChatOptions, ReasoningEffort};
use crate::tool::Tool;

// ============================================================================
// Shared tool definition types (for Chat Completions API format)
// ============================================================================

#[derive(Clone, serde::Serialize)]
pub(crate) struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: &'static str,
    pub function: FunctionDefinition,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// ============================================================================
// Shared helpers
// ============================================================================

/// Convert `ReasoningEffort` enum to the API string representation.
pub(crate) fn effort_to_str(effort: &ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::XHigh => "xhigh",
        ReasoningEffort::High => "high",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Minimal => "minimal",
    }
}

/// Normalize a schemars Schema into OpenAI-compatible JSON.
/// OpenAI requires `type: "object"` and `properties` even for empty params.
pub(crate) fn normalize_schema_for_openai(schema: &schemars::Schema) -> serde_json::Value {
    let mut value = serde_json::to_value(schema).unwrap_or(serde_json::json!({}));

    if let Some(obj) = value.as_object_mut() {
        if !obj.contains_key("type") {
            obj.insert("type".to_string(), serde_json::json!("object"));
        }
        if !obj.contains_key("properties") {
            obj.insert("properties".to_string(), serde_json::json!({}));
        }
    }

    value
}

/// Build tool definitions from registered tools (Chat Completions format).
pub(crate) fn build_tool_defs(tools: &[Arc<Tool>]) -> Option<Vec<ToolDefinition>> {
    if tools.is_empty() {
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
                        parameters: normalize_schema_for_openai(&t.param_schema),
                    },
                })
                .collect(),
        )
    }
}

/// Build a `ReasoningRequest` from `ChatOptions` for the OpenRouter/Chat Completions format.
pub(crate) fn build_reasoning_request(options: &ChatOptions) -> Option<ReasoningRequest> {
    if options.reasoning_effort.is_none() && options.reasoning_budget.is_none() && !options.exclude_reasoning {
        return None;
    }
    Some(ReasoningRequest {
        effort: options.reasoning_effort.as_ref().map(effort_to_str),
        max_tokens: options.reasoning_budget,
        exclude: options.exclude_reasoning,
    })
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(serde::Serialize)]
pub(crate) struct ReasoningRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "is_false")]
    pub exclude: bool,
}
