pub mod conversation;
pub mod llm;
pub mod permission;
pub mod tool;

/// Re-export the `#[tool]` proc-macro for defining LLM tools.
pub use llm_rs_macros::tool;

#[cfg(test)]
mod tool_tests;

#[cfg(test)]
mod llm_tests;

#[cfg(test)]
mod conversation_tests;

#[cfg(test)]
mod permission_tests;
