pub mod conversation;
pub mod llm;
pub mod tool;

/// Re-export the `#[tool]` proc-macro for defining LLM tools.
pub use llm_rs_macros::tool;

#[cfg(test)]
mod tool_tests;