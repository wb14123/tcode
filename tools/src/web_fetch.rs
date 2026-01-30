use headless_chrome::{Browser, LaunchOptions};
use llm_rs_macros::tool;

const READABILITY_JS: &str = include_str!("vendor/readability-0.6.0.js");

/// Fetch a web page using headless Chrome and extract clean HTML using Readability.js.
fn fetch_and_extract(url: &str) -> Result<String, String> {
    // Launch browser
    let browser = Browser::new(LaunchOptions::default()).map_err(|e| e.to_string())?;

    // Create a new tab
    let tab = browser.new_tab().map_err(|e| e.to_string())?;

    // Navigate to the URL
    tab.navigate_to(url).map_err(|e| e.to_string())?;

    // Wait for the page to load (wait for body element)
    tab.wait_for_element("body").map_err(|e| e.to_string())?;

    // Inject Readability.js
    tab.evaluate(READABILITY_JS, false)
        .map_err(|e| e.to_string())?;

    // Execute Readability to parse the document and return cleaned HTML
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

    // Extract the string value from the result
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
    let result = fetch_and_extract(&url);
    let output = result.unwrap_or_else(|e| format!("Error: {}", e));
    tokio_stream::once(output)
}
