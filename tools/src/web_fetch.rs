use headless_chrome::{Browser, LaunchOptions};
use llm_rs_macros::tool;

const READABILITY_JS: &str = include_str!("vendor/readability-0.6.0.js");

/// Fetch a web page using headless Chrome and extract clean HTML using Readability.js.
fn fetch_and_extract(url: &str) -> Result<String, String> {
    // SAFETY: Remove LD_PRELOAD to bypass proxychains4 - Chrome's multi-process architecture
    // doesn't work with proxychains4's LD_PRELOAD interception. This is called from
    // spawn_blocking before Chrome subprocess is launched. While not thread-safe, the
    // variable is only relevant for subprocess spawning, not for other threads.
    unsafe { std::env::remove_var("LD_PRELOAD") };

    let browser = Browser::new(LaunchOptions::default()).map_err(|e| e.to_string())?;
    let tab = browser.new_tab().map_err(|e| e.to_string())?;

    tab.navigate_to(url).map_err(|e| e.to_string())?;
    tab.wait_for_element("body").map_err(|e| e.to_string())?;

    tab.evaluate(READABILITY_JS, false).map_err(|e| e.to_string())?;

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

    let result = tab.evaluate(js_code, false).map_err(|e| e.to_string())?;

    match result.value {
        Some(serde_json::Value::String(content)) => Ok(content),
        Some(serde_json::Value::Null) | None => {
            Err("Readability could not extract content from this page".to_string())
        }
        Some(other) => Err(format!("Unexpected result type: {:?}", other)),
    }
}

/// Fetch a web page and return cleaned HTML content extracted by Readability
#[tool]
pub fn web_fetch(
    /// The URL to fetch and extract content from
    url: String,
) -> impl tokio_stream::Stream<Item = String> {
    async_stream::stream! {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::task::spawn_blocking(move || fetch_and_extract(&url)),
        )
        .await;

        let output = match result {
            Ok(Ok(Ok(content))) => content,
            Ok(Ok(Err(e))) => format!("Error: {}", e),
            Ok(Err(e)) => format!("Error: spawn_blocking join error: {}", e),
            Err(e) => format!("Error: web_fetch timed out after 30s: {}", e),
        };
        yield output;
    }
}
