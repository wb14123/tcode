//! Tool system for defining and executing LLM tools with streaming output.
//!
//! # Example
//!
//! ```rust
//! use schemars::JsonSchema;
//! use serde::Deserialize;
//! use llm_rs::tool::{Tool, ToolContext};
//!
//! #[derive(Deserialize, JsonSchema)]
//! struct ReadFileParams {
//!     /// The file path to read
//!     path: String,
//! }
//!
//! // Create a tool - handler returns Stream<Item = Result<T, E>>
//! let tool = Tool::new(
//!     "read_file",
//!     "Read a file's contents",
//!     None, // no timeout
//!     |_ctx: ToolContext, params: ReadFileParams| {
//!         tokio_stream::once(Ok::<_, String>(format!("Reading {}", params.path)))
//!     },
//! );
//!
//! // Execute with JSON string
//! use tokio_util::sync::CancellationToken;
//! let ctx = ToolContext { cancel_token: CancellationToken::new(), permission: llm_rs::permission::ScopedPermissionManager::always_allow("test") };
//! let stream = tool.execute(ctx, r#"{"path": "/tmp/test.txt"}"#.to_string());
//! ```

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use pin_project_lite::pin_project;
use schemars::Schema;
use serde::de::DeserializeOwned;

use tokio::time::{Instant, Sleep};
use tokio_stream::{Stream, StreamExt};

pub use tokio_util::sync::CancellationToken;

/// Context provided to every tool execution.
/// Extensible — future additions (user info, etc.) go here.
#[derive(Clone)]
pub struct ToolContext {
    pub cancel_token: CancellationToken,
    /// Scoped permission manager for this tool.
    pub permission: crate::permission::ScopedPermissionManager,
}

/// Type alias for the boxed stream returned by tool execution.
pub type ToolOutputStream = Pin<Box<dyn Stream<Item = String> + Send>>;

pin_project! {
    /// A stream wrapper that enforces a total timeout across all items.
    /// The deadline is dynamically extended by any time the tool spends
    /// waiting for user permission approval, so approval wait time does
    /// not count against the timeout.
    struct TimeoutStream<S> {
        #[pin]
        inner: S,
        #[pin]
        deadline: Sleep,
        timed_out: bool,
        timeout_duration: Duration,
        approval_pending: Arc<AtomicBool>,
    }
}

impl<S: Stream<Item = String>> Stream for TimeoutStream<S> {
    type Item = String;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        // If already timed out, return None
        if *this.timed_out {
            return Poll::Ready(None);
        }

        // While approval is pending, keep resetting the deadline so the
        // timeout only counts actual tool execution time.
        if this.approval_pending.load(Ordering::Acquire) {
            this.deadline
                .as_mut()
                .reset(Instant::now() + *this.timeout_duration);
        }

        // Check if deadline has passed
        if this.deadline.as_mut().poll(cx).is_ready() {
            // Double-check: approval may have become pending between the
            // check above and the deadline firing. If so, reset instead of
            // timing out.
            if this.approval_pending.load(Ordering::Acquire) {
                this.deadline
                    .as_mut()
                    .reset(Instant::now() + *this.timeout_duration);
            } else {
                *this.timed_out = true;
                let msg = format!(
                    "Error: Tool execution timed out after {}ms",
                    this.timeout_duration.as_millis()
                );
                return Poll::Ready(Some(msg));
            }
        }

        // Poll the inner stream
        this.inner.poll_next(cx)
    }
}

pin_project! {
    /// A stream wrapper that stops yielding when a CancellationToken is cancelled.
    struct CancellableStream<S> {
        #[pin]
        inner: S,
        cancel_token: CancellationToken,
        cancelled: bool,
    }
}

impl<S: Stream<Item = String>> Stream for CancellableStream<S> {
    type Item = String;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        if *this.cancelled {
            return Poll::Ready(None);
        }

        // Check cancellation by polling the cancelled() future.
        // We create a new future each time since CancellationToken::cancelled() is cheap.
        let cancelled_fut = this.cancel_token.cancelled();
        tokio::pin!(cancelled_fut);
        if cancelled_fut.poll(cx).is_ready() {
            *this.cancelled = true;
            return Poll::Ready(Some(
                "Error: Tool execution was cancelled by the user. Do not retry this tool call. Instead, ask the user what they would like to do.".to_string(),
            ));
        }

        // Poll the inner stream
        this.inner.poll_next(cx)
    }
}

/// Normalize a schemars `Schema` into provider-compatible JSON.
/// Ensures `type: "object"` and `properties: {}` are present, as required
/// by both OpenAI and Claude APIs for tool parameter schemas.
pub fn normalize_schema(schema: &Schema) -> serde_json::Value {
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

/// Marker trait for valid tool parameter types.
///
/// Automatically implemented for types that are:
/// - `DeserializeOwned` - Can be deserialized from JSON
/// - `JsonSchema` - Can generate a JSON schema
/// - `Send + Sync + 'static` - Can be shared across threads
pub trait ToolParams: DeserializeOwned + schemars::JsonSchema + Send + Sync + 'static {}
impl<T> ToolParams for T where T: DeserializeOwned + schemars::JsonSchema + Send + Sync + 'static {}

/// A tool that can be called by an LLM.
///
/// Tools have a name, description, parameter schema, and a handler function
/// that takes JSON string input and returns a stream of Result outputs.
///
/// # Example
///
/// ```rust
/// use schemars::JsonSchema;
/// use serde::Deserialize;
/// use llm_rs::tool::{Tool, ToolContext};
///
/// #[derive(Deserialize, JsonSchema)]
/// struct Params {
///     /// The query to search for
///     query: String,
///     #[serde(default)]
///     limit: Option<u32>,
/// }
///
/// let tool = Tool::new(
///     "search",
///     "Search the codebase",
///     None, // no timeout
///     |_ctx: ToolContext, params: Params| {
///         tokio_stream::once(Ok::<_, String>(format!("Searching for: {}", params.query)))
///     },
/// );
///
/// // Execute with JSON string
/// use tokio_util::sync::CancellationToken;
/// let ctx = ToolContext { cancel_token: CancellationToken::new(), permission: llm_rs::permission::ScopedPermissionManager::always_allow("test") };
/// let stream = tool.execute(ctx, r#"{"query": "foo"}"#.to_string());
/// ```
pub struct Tool {
    /// Unique name for the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub param_schema: Schema,
    /// Optional timeout for tool execution.
    pub timeout: Option<Duration>,
    handler: Box<dyn Fn(ToolContext, String) -> ToolOutputStream + Send + Sync>,
}

impl Tool {
    /// Create a new tool.
    ///
    /// The handler function receives a `ToolContext` and typed parameters,
    /// and returns a stream of Results.
    /// JSON deserialization is handled automatically. Errors are formatted with "Error: " prefix.
    ///
    /// # Type Parameters
    /// - `P`: Parameter type (must implement `Deserialize` + `JsonSchema`)
    /// - `F`: Handler function type
    /// - `S`: Stream type returned by handler
    /// - `T`: Success type (must implement `ToString`)
    /// - `E`: Error type (must implement `ToString`)
    ///
    /// # Arguments
    /// - `name`: Unique tool name (used by LLM to call the tool)
    /// - `description`: Human-readable description
    /// - `timeout`: Optional timeout for tool execution
    /// - `handler`: Function that takes `(ToolContext, params)`, returns a stream of `Result<T, E>`
    pub fn new<P, F, S, T, E>(
        name: impl Into<String>,
        description: impl Into<String>,
        timeout: Option<Duration>,
        handler: F,
    ) -> Self
    where
        P: ToolParams,
        F: Fn(ToolContext, P) -> S + Send + Sync + 'static,
        S: Stream<Item = Result<T, E>> + Send + 'static,
        T: ToString + Send + 'static,
        E: ToString + Send + 'static,
    {
        Tool {
            name: name.into(),
            description: description.into(),
            param_schema: schemars::schema_for!(P),
            timeout,
            handler: Box::new(move |ctx: ToolContext, json_str: String| {
                let json_str = if json_str.trim().is_empty() {
                    "{}".to_string()
                } else {
                    json_str
                };
                match serde_json::from_str::<P>(&json_str) {
                    Ok(params) => Box::pin(handler(ctx, params).map(|item| match item {
                        Ok(v) => v.to_string(),
                        Err(e) => format!("Error: {}", e.to_string()),
                    })),
                    Err(e) => {
                        let error = format!("Error: Failed to parse tool arguments: {}", e);
                        Box::pin(tokio_stream::once(error))
                    }
                }
            }),
        }
    }

    /// Create a sentinel tool with a no-op handler.
    ///
    /// Sentinel tools are registered with the LLM so it sees their schemas,
    /// but their execution is intercepted in `execute_tool_calls` rather than
    /// going through the regular `tool.execute()` path.
    pub fn new_sentinel(
        name: impl Into<String>,
        description: impl Into<String>,
        param_schema: Schema,
    ) -> Self {
        Tool {
            name: name.into(),
            description: description.into(),
            param_schema,
            timeout: None,
            handler: Box::new(|_ctx, _| {
                Box::pin(tokio_stream::once(
                    "Error: This tool's execution is handled internally".to_string(),
                ))
            }),
        }
    }

    /// Execute the tool with a JSON string argument.
    ///
    /// Returns a stream of string outputs that can be consumed incrementally.
    /// The stream is always wrapped with `CancellableStream` so cancellation
    /// stops output. If a timeout is set, `TimeoutStream` is also applied.
    pub fn execute(&self, ctx: ToolContext, arguments: String) -> ToolOutputStream {
        let cancel_token = ctx.cancel_token.clone();
        let approval_pending = ctx.permission.approval_pending();
        let stream = (self.handler)(ctx, arguments);

        // Apply timeout if configured. The deadline is dynamically paused
        // while the tool is waiting for user permission approval.
        let stream: ToolOutputStream = match self.timeout {
            Some(timeout) => Box::pin(TimeoutStream {
                inner: stream,
                deadline: tokio::time::sleep(timeout),
                timed_out: false,
                timeout_duration: timeout,
                approval_pending,
            }),
            None => stream,
        };

        // Always wrap with CancellableStream
        Box::pin(CancellableStream {
            inner: stream,
            cancel_token,
            cancelled: false,
        })
    }
}
