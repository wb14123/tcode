use std::fmt::Write;

use anyhow::{Result, anyhow};
use serde::Deserialize;

use crate::SearchEngineKind;
use crate::browser;

const EXTRACT_SEARCH_RESULTS_JS: &str = include_str!("extract-search-results.js");

/// A single search result returned by Kagi search (internal parsing type).
#[derive(Debug, Clone, Deserialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    sub_results: Vec<SubResult>,
}

/// A sub-result within a Kagi search result (internal parsing type).
#[derive(Debug, Clone, Deserialize)]
struct SubResult {
    title: String,
    url: String,
    snippet: String,
}

/// Format Kagi search results into human-readable text.
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

/// Dispatch a web search to the appropriate engine.
pub fn search(query: &str, engine: SearchEngineKind) -> Result<String> {
    match engine {
        SearchEngineKind::Kagi => search_kagi(query),
        SearchEngineKind::Google => search_google(query),
    }
}

/// Perform a web search via Kagi and return formatted text results.
/// Retries once if the browser connection is lost.
fn search_kagi(query: &str) -> Result<String> {
    match search_kagi_inner(query) {
        Ok(result) => Ok(result),
        Err(e) if e.to_string().contains("connection is closed") => {
            tracing::warn!("Browser connection lost during web_search, restarting: {e}");
            browser::shutdown_browser();
            search_kagi_inner(query)
        }
        Err(e) => Err(e),
    }
}

fn search_kagi_inner(query: &str) -> Result<String> {
    let encoded = urlencoding::encode(query);
    let url = format!("https://kagi.com/search?q={encoded}");

    let tab = browser::open_tab(&url)?;

    let result = tab.evaluate(EXTRACT_SEARCH_RESULTS_JS, false)?;

    match result.value {
        Some(serde_json::Value::String(json_str)) => {
            let results: Vec<SearchResult> = serde_json::from_str(&json_str)
                .map_err(|e| anyhow!("Failed to parse search results: {e}"))?;
            format_results(&results)
        }
        Some(serde_json::Value::Null) | None => {
            Err(anyhow!("Could not extract search results from the page"))
        }
        Some(other) => Err(anyhow!("Unexpected result type: {:?}", other)),
    }
}

/// Perform a web search via Google using accessibility tree extraction.
fn search_google(query: &str) -> Result<String> {
    let encoded = urlencoding::encode(query);
    let url = format!("https://www.google.com/search?q={encoded}");
    crate::web_fetch::fetch_and_extract(&url)
}
