# llm-rs-macros

Procedural macro crate providing the `#[tool]` attribute macro for defining LLM tools with minimal boilerplate.

## Usage

The `#[tool]` macro transforms a function into a `Tool` struct. It automatically generates:

1. A typed **params struct** (with `Deserialize` + `JsonSchema` derives) from the function's parameters
2. A **`{fn_name}_tool()`** constructor function that returns a configured `Tool`

```rust
use llm_rs_macros::tool;

/// Fetch the contents of a web page
#[tool(timeout_ms = 300000)]
fn web_fetch(
    /// The URL to fetch
    url: String,
) -> impl Stream<Item = Result<String, anyhow::Error>> {
    // implementation...
}

// Generated: web_fetch_tool() -> Tool
```

- **Doc comments** on the function become the tool's description for the LLM.
- **Doc comments** on parameters become field descriptions in the JSON schema.
- **`timeout_ms`** (optional): Sets the tool execution timeout in milliseconds.
- The function must return `impl Stream<Item = Result<T, E>>` where `T: Serialize`.
- Async functions are not supported (the macro wraps the body in `async_stream`).
