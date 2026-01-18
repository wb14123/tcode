//! Tool system for defining and executing LLM tools with streaming output.
//!
//! # Example
//!
//! ```rust
//! use schemars::JsonSchema;
//! use serde::Deserialize;
//! use llm_rs::tool::{Tool, ToolSchema};
//!
//! #[derive(Deserialize, JsonSchema)]
//! struct ReadFileParams {
//!     /// The file path to read
//!     path: String,
//! }
//!
//! // Create a tool with full type information
//! let tool = Tool::new(
//!     "read_file",
//!     "Read a file's contents",
//!     |params: ReadFileParams| {
//!         tokio_stream::once(format!("Reading {}", params.path))
//!     },
//! );
//!
//! // Convert to schema for LLM API integration
//! let schema: ToolSchema = tool.to_schema();
//!
//! // Execute with JSON string
//! let stream = schema.execute(r#"{"path": "/tmp/test.txt"}"#.to_string());
//! ```

use std::marker::PhantomData;
use std::pin::Pin;

use schemars::Schema;
use serde::de::DeserializeOwned;
use tokio_stream::{Stream, StreamExt};

/// Type alias for the boxed stream returned by tool schema execution.
pub type ToolOutputStream = Pin<Box<dyn Stream<Item = String> + Send>>;

/// Marker trait for valid tool parameter types.
///
/// Automatically implemented for types that are:
/// - `DeserializeOwned` - Can be deserialized from JSON
/// - `JsonSchema` - Can generate a JSON schema
/// - `Send + Sync + 'static` - Can be shared across threads
pub trait ToolParams: DeserializeOwned + schemars::JsonSchema + Send + Sync + 'static {}
impl<T> ToolParams for T where T: DeserializeOwned + schemars::JsonSchema + Send + Sync + 'static {}

/// A tool that stores the handler function with full type information.
///
/// Use `to_schema()` to convert to a `ToolSchema` for LLM API integration.
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
/// // Convert to schema for LLM API
/// let schema = tool.to_schema();
/// ```
pub struct Tool<P, F, S, T> {
    /// Unique name for the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    handler: F,
    _marker: PhantomData<fn(P) -> (S, T)>,
}

impl<P, F, S, T> Tool<P, F, S, T>
where
    P: ToolParams,
    F: Fn(P) -> S + Send + Sync + 'static,
    S: Stream<Item = T> + Send + 'static,
    T: ToString + Send + 'static,
{
    /// Create a new tool.
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
    pub fn new(name: impl Into<String>, description: impl Into<String>, handler: F) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            handler,
            _marker: PhantomData,
        }
    }

    /// Execute the tool directly with typed parameters.
    ///
    /// Returns the stream produced by the handler.
    pub fn execute(&self, params: P) -> S {
        (self.handler)(params)
    }

    /// Convert to a `ToolSchema` for LLM API integration.
    ///
    /// The schema stores:
    /// - JSON schema for parameters
    /// - Handler that accepts JSON string input and outputs string stream
    pub fn to_schema(self) -> ToolSchema {
        let handler = self.handler;
        ToolSchema {
            name: self.name,
            description: self.description,
            parameters: schemars::schema_for!(P),
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
}

/// LLM API friendly tool schema with JSON schema parameters and string-based handler.
///
/// Created by calling `Tool::to_schema()`. This struct is suitable for:
/// - Serializing tool definitions for LLM APIs
/// - Executing tools with JSON string arguments
pub struct ToolSchema {
    /// Unique name for the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's parameters.
    pub parameters: Schema,
    handler: Box<dyn Fn(String) -> ToolOutputStream + Send + Sync>,
}

impl ToolSchema {
    /// Execute the tool with a JSON string argument.
    ///
    /// Returns a stream of string outputs that can be consumed incrementally.
    pub fn execute(&self, arguments: String) -> ToolOutputStream {
        (self.handler)(arguments)
    }
}
