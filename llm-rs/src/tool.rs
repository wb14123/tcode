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
//! // Create a tool
//! let tool = Tool::new(
//!     "read_file",
//!     "Read a file's contents",
//!     |params: ReadFileParams| {
//!         tokio_stream::once(format!("Reading {}", params.path))
//!     },
//! );
//!
//! // Execute with JSON string
//! let stream = tool.execute(r#"{"path": "/tmp/test.txt"}"#.to_string());
//! ```

use std::pin::Pin;

use schemars::Schema;
use serde::de::DeserializeOwned;
use tokio_stream::{Stream, StreamExt};

/// Type alias for the boxed stream returned by tool execution.
pub type ToolOutputStream = Pin<Box<dyn Stream<Item = String> + Send>>;

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
/// that takes JSON string input and returns a stream of string outputs.
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
///     |params: Params| {
///         tokio_stream::once(format!("Searching for: {}", params.query))
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
    handler: Box<dyn Fn(String) -> ToolOutputStream + Send + Sync>,
}

impl Tool {
    /// Create a new tool.
    ///
    /// The handler function receives typed parameters and returns a stream.
    /// JSON deserialization is handled automatically.
    ///
    /// # Type Parameters
    /// - `P`: Parameter type (must implement `Deserialize` + `JsonSchema`)
    /// - `F`: Handler function type
    /// - `S`: Stream type returned by handler
    /// - `T`: Item type of the stream (must implement `ToString`)
    ///
    /// # Arguments
    /// - `name`: Unique tool name (used by LLM to call the tool)
    /// - `description`: Human-readable description
    /// - `handler`: Function that takes params, returns any stream of `ToString` items
    pub fn new<P, F, S, T>(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: F,
    ) -> Self
    where
        P: ToolParams,
        F: Fn(P) -> S + Send + Sync + 'static,
        S: Stream<Item = T> + Send + 'static,
        T: ToString + Send + 'static,
    {
        Tool {
            name: name.into(),
            description: description.into(),
            param_schema: schemars::schema_for!(P),
            handler: Box::new(move |json_str: String| {
                match serde_json::from_str::<P>(&json_str) {
                    Ok(params) => Box::pin(handler(params).map(|item| item.to_string())),
                    Err(e) => {
                        let error = format!("Error: Failed to parse tool arguments: {}", e);
                        Box::pin(tokio_stream::once(error))
                    }
                }
            }),
        }
    }

    /// Execute the tool with a JSON string argument.
    ///
    /// Returns a stream of string outputs that can be consumed incrementally.
    pub fn execute(&self, arguments: String) -> ToolOutputStream {
        (self.handler)(arguments)
    }
}
