//! Tool system for defining and executing LLM tools with streaming output.
//!
//! # Example
//!
//! ```rust
//! use schemars::JsonSchema;
//! use serde::Deserialize;
//! use llm_rs::tool::Tool;
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
//!     |params: ReadFileParams| {
//!         tokio_stream::once(Ok::<_, String>(format!("Reading {}", params.path)))
//!     },
//! );
//!
//! // Execute with JSON string
//! let stream = tool.execute(r#"{"path": "/tmp/test.txt"}"#.to_string());
//! ```

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use pin_project_lite::pin_project;
use schemars::Schema;
use serde::de::DeserializeOwned;
use tokio::time::Sleep;
use tokio_stream::{Stream, StreamExt};

/// Type alias for the boxed stream returned by tool execution.
pub type ToolOutputStream = Pin<Box<dyn Stream<Item = String> + Send>>;

pin_project! {
    /// A stream wrapper that enforces a total timeout across all items.
    struct TimeoutStream<S> {
        #[pin]
        inner: S,
        #[pin]
        deadline: Sleep,
        timed_out: bool,
        timeout_duration: Duration,
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

        // Check if deadline has passed
        if this.deadline.as_mut().poll(cx).is_ready() {
            *this.timed_out = true;
            let msg = format!(
                "Error: Tool execution timed out after {}ms",
                this.timeout_duration.as_millis()
            );
            return Poll::Ready(Some(msg));
        }

        // Poll the inner stream
        this.inner.poll_next(cx)
    }
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
/// use llm_rs::tool::Tool;
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
///     |params: Params| {
///         tokio_stream::once(Ok::<_, String>(format!("Searching for: {}", params.query)))
///     },
/// );
///
/// // Execute with JSON string
/// let stream = tool.execute(r#"{"query": "foo"}"#.to_string());
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
    handler: Box<dyn Fn(String) -> ToolOutputStream + Send + Sync>,
}

impl Tool {
    /// Create a new tool.
    ///
    /// The handler function receives typed parameters and returns a stream of Results.
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
    /// - `handler`: Function that takes params, returns a stream of `Result<T, E>`
    pub fn new<P, F, S, T, E>(
        name: impl Into<String>,
        description: impl Into<String>,
        timeout: Option<Duration>,
        handler: F,
    ) -> Self
    where
        P: ToolParams,
        F: Fn(P) -> S + Send + Sync + 'static,
        S: Stream<Item = Result<T, E>> + Send + 'static,
        T: ToString + Send + 'static,
        E: ToString + Send + 'static,
    {
        Tool {
            name: name.into(),
            description: description.into(),
            param_schema: schemars::schema_for!(P),
            timeout,
            handler: Box::new(move |json_str: String| {
                let json_str = if json_str.trim().is_empty() { "{}".to_string() } else { json_str };
                match serde_json::from_str::<P>(&json_str) {
                    Ok(params) => Box::pin(handler(params).map(|item| match item {
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
            handler: Box::new(|_| {
                Box::pin(tokio_stream::once(
                    "Error: This tool's execution is handled internally".to_string(),
                ))
            }),
        }
    }

    /// Execute the tool with a JSON string argument.
    ///
    /// Returns a stream of string outputs that can be consumed incrementally.
    /// If a timeout is set, the stream will yield an error and terminate if
    /// the total execution time exceeds the timeout.
    pub fn execute(&self, arguments: String) -> ToolOutputStream {
        let stream = (self.handler)(arguments);

        match self.timeout {
            Some(timeout) => Box::pin(TimeoutStream {
                inner: stream,
                deadline: tokio::time::sleep(timeout),
                timed_out: false,
                timeout_duration: timeout,
            }),
            None => stream,
        }
    }
}
