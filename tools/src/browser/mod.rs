use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use headless_chrome::{Browser, LaunchOptions, Tab};

const WAIT_FOR_IDLE_JS: &str = include_str!("wait-for-idle.js");

/// Get the Chrome user data directory for persistent sessions.
pub fn chrome_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tcode")
        .join("chrome")
}

/// Navigate to a URL using headless Chrome and wait for the page to fully load.
///
/// Returns the browser tab ready for further JS evaluation. The caller is
/// responsible for evaluating any additional JavaScript on the tab.
pub fn navigate_and_wait(url: &str) -> Result<(Browser, Arc<Tab>)> {
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
    tab.wait_until_navigated()?;

    // Wait for document.readyState to be 'complete' and network to be idle.
    tab.evaluate(WAIT_FOR_IDLE_JS, true)?;

    Ok((browser, tab))
}
