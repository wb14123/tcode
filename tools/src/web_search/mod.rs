use std::fmt::Write;

use anyhow::{anyhow, Result};
use browser_server::SearchResult;
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;

use crate::browser_client;

fn format_results(results: &[SearchResult]) -> Result<String> {
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        writeln!(out, "{}. {}", i + 1, r.title)?;
        writeln!(out, "   {}", r.url)?;
        if !r.snippet.is_empty() {
            writeln!(out, "   {}", r.snippet)?;
        }
        for sub in &r.sub_results {
            writeln!(out, "   - {}", sub.title)?;
            writeln!(out, "     {}", sub.url)?;
            if !sub.snippet.is_empty() {
                writeln!(out, "     {}", sub.snippet)?;
            }
        }
        if i + 1 < results.len() {
            writeln!(out)?;
        }
    }
    Ok(out)
}

/// Search the web using Kagi and return structured search results
#[tool(timeout_ms = 300000)]
pub fn web_search(
    ctx: ToolContext,
    /// The search query
    query: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let client = match browser_client::get_global_client() {
            Some(c) => c,
            None => {
                yield Err(anyhow!("Browser client not initialized. Is the browser-server running?"));
                return;
            }
        };

        tokio::select! {
            result = client.web_search(&query) => {
                match result {
                    Ok(results) => {
                        if results.is_empty() {
                            yield Ok("No search results found.".to_string());
                        } else {
                            yield Ok(format_results(&results)?);
                        }
                    }
                    Err(e) => yield Err(e),
                }
            }
            _ = ctx.cancel_token.cancelled() => {
                yield Err(anyhow!("Cancelled"));
            }
        }
    }
}
