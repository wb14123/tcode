use std::fmt::Write;

use anyhow::{anyhow, Result};
use llm_rs::tool::ToolContext;
use llm_rs_macros::tool;
use serde::Deserialize;

use crate::browser;

const EXTRACT_SEARCH_RESULTS_JS: &str = include_str!("extract-search-results.js");

#[derive(Deserialize)]
struct SubResult {
    title: String,
    url: String,
    snippet: String,
}

#[derive(Deserialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    sub_results: Vec<SubResult>,
}

fn format_results(results: &[SearchResult]) -> String {
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        let _ = writeln!(out, "{}. {}", i + 1, r.title);
        let _ = writeln!(out, "   {}", r.url);
        if !r.snippet.is_empty() {
            let _ = writeln!(out, "   {}", r.snippet);
        }
        for sub in &r.sub_results {
            let _ = writeln!(out, "   - {}", sub.title);
            let _ = writeln!(out, "     {}", sub.url);
            if !sub.snippet.is_empty() {
                let _ = writeln!(out, "     {}", sub.snippet);
            }
        }
        if i + 1 < results.len() {
            let _ = writeln!(out);
        }
    }
    out
}

fn search_and_extract(query: &str) -> Result<String> {
    let encoded = urlencoding::encode(query);
    let url = format!("https://kagi.com/search?q={encoded}");

    let tab = browser::open_tab(&url)?;

    let result = tab.evaluate(EXTRACT_SEARCH_RESULTS_JS, false)?;

    match result.value {
        Some(serde_json::Value::String(json_str)) => {
            let results: Vec<SearchResult> = serde_json::from_str(&json_str)
                .map_err(|e| anyhow!("Failed to parse search results: {e}"))?;
            if results.is_empty() {
                Ok("No search results found.".to_string())
            } else {
                Ok(format_results(&results))
            }
        }
        Some(serde_json::Value::Null) | None => {
            Err(anyhow!("Could not extract search results from the page"))
        }
        Some(other) => Err(anyhow!("Unexpected result type: {:?}", other)),
    }
}

/// Search the web using Kagi and return structured search results
#[tool(timeout_ms = 300000)]
pub fn web_search(
    ctx: ToolContext,
    /// The search query
    query: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        let handle = tokio::task::spawn_blocking(move || search_and_extract(&query));
        tokio::select! {
            result = handle => {
                yield result.map_err(anyhow::Error::from).flatten();
            }
            _ = ctx.cancel_token.cancelled() => {
                yield Err(anyhow!("Cancelled"));
            }
        }
    }
}
