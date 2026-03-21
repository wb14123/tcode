use anyhow::{Result, anyhow};

use crate::SearchResult;
use crate::browser;

const EXTRACT_SEARCH_RESULTS_JS: &str = include_str!("extract-search-results.js");

/// Perform a web search via Kagi and extract structured results.
/// Retries once if the browser connection is lost.
pub fn search_and_extract(query: &str) -> Result<Vec<SearchResult>> {
    match search_and_extract_inner(query) {
        Ok(result) => Ok(result),
        Err(e) if e.to_string().contains("connection is closed") => {
            tracing::warn!("Browser connection lost during web_search, restarting: {e}");
            browser::shutdown_browser();
            search_and_extract_inner(query)
        }
        Err(e) => Err(e),
    }
}

fn search_and_extract_inner(query: &str) -> Result<Vec<SearchResult>> {
    let encoded = urlencoding::encode(query);
    let url = format!("https://kagi.com/search?q={encoded}");

    let tab = browser::open_tab(&url)?;

    let result = tab.evaluate(EXTRACT_SEARCH_RESULTS_JS, false)?;

    match result.value {
        Some(serde_json::Value::String(json_str)) => {
            let results: Vec<SearchResult> = serde_json::from_str(&json_str)
                .map_err(|e| anyhow!("Failed to parse search results: {e}"))?;
            Ok(results)
        }
        Some(serde_json::Value::Null) | None => {
            Err(anyhow!("Could not extract search results from the page"))
        }
        Some(other) => Err(anyhow!("Unexpected result type: {:?}", other)),
    }
}
