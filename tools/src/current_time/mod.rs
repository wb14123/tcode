use anyhow::Result;
use chrono::Local;
use llm_rs_macros::tool;

/// Get the current local date and time
#[tool]
pub fn current_time() -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let now = Local::now();
        yield Ok(now.format("%Y-%m-%d %H:%M:%S %Z").to_string());
    }
}
