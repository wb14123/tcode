use std::path::PathBuf;

use anyhow::{anyhow, Result};
use headless_chrome::{Browser, LaunchOptions};
use llm_rs_macros::tool;

const READABILITY_JS: &str = include_str!("vendor/readability-0.6.0.js");

/// Get the Chrome user data directory for persistent sessions.
pub fn chrome_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tcode")
        .join("chrome")
}

/// Fetch a web page using headless Chrome and extract clean HTML using Readability.js.
fn fetch_and_extract(url: &str) -> Result<String> {
    // SAFETY: Remove LD_PRELOAD to bypass proxychains4 - Chrome's multi-process architecture
    // doesn't work with proxychains4's LD_PRELOAD interception. This is called from
    // spawn_blocking before Chrome subprocess is launched. While not thread-safe, the
    // variable is only relevant for subprocess spawning, not for other threads.
    unsafe { std::env::remove_var("LD_PRELOAD") };

    let data_dir = chrome_data_dir();
    std::fs::create_dir_all(&data_dir)?;

    let launch_options = LaunchOptions {
        user_data_dir: Some(data_dir),
        headless: false,
        ..LaunchOptions::default()
    };
    let browser = Browser::new(launch_options)?;
    let tab = browser.new_tab()?;

    tab.navigate_to(url)?;
    tab.wait_for_element("body")?;
    tab.evaluate(READABILITY_JS, false)?;

    let js_code = r#"
        (function() {
            var documentClone = document.cloneNode(true);
            var article = new Readability(documentClone).parse();
            if (article && article.content) {
                return article.content;
            }
            return null;
        })()
    "#;

    let result = tab.evaluate(js_code, false)?;

    match result.value {
        Some(serde_json::Value::String(content)) => Ok(content),
        Some(serde_json::Value::Null) | None => {
            Err(anyhow!("Readability could not extract content from this page"))
        }
        Some(other) => Err(anyhow!("Unexpected result type: {:?}", other)),
    }
}

/// Fetch a web page and return cleaned HTML content extracted by Readability
#[tool(timeout_ms = 30000)]
pub fn web_fetch(
    /// The URL to fetch and extract content from
    url: String,
) -> impl tokio_stream::Stream<Item = Result<String>> {
    async_stream::stream! {
        yield tokio::task::spawn_blocking(move || fetch_and_extract(&url))
            .await
            .map_err(anyhow::Error::from)
            .flatten()
    }
}
