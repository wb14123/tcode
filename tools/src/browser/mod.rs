use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use headless_chrome::{Browser, LaunchOptions, Tab};
use tracing::warn;

const WAIT_FOR_IDLE_JS: &str = include_str!("wait-for-idle.js");
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

struct BrowserState {
    browser: Option<Browser>,
    active_tabs: usize,
    last_activity: Instant,
    idle_timeout: Duration,
}

static BROWSER_STATE: LazyLock<Mutex<BrowserState>> = LazyLock::new(|| {
    Mutex::new(BrowserState {
        browser: None,
        active_tabs: 0,
        last_activity: Instant::now(),
        idle_timeout: DEFAULT_IDLE_TIMEOUT,
    })
});

static CHECKER_RUNNING: AtomicBool = AtomicBool::new(false);

/// Get the Chrome user data directory for persistent sessions.
pub fn chrome_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tcode")
        .join("chrome")
}

/// Configure the idle timeout for the shared browser.
/// Must be called before the first `open_tab` to take effect.
pub fn set_idle_timeout(timeout: Duration) {
    let mut state = BROWSER_STATE.lock().unwrap();
    state.idle_timeout = timeout;
}

/// RAII guard that derefs to `Tab`. On drop, closes the tab and decrements
/// the active tab count in the shared browser state.
pub struct TabGuard {
    tab: Arc<Tab>,
}

impl Deref for TabGuard {
    type Target = Tab;
    fn deref(&self) -> &Tab {
        &self.tab
    }
}

impl Drop for TabGuard {
    fn drop(&mut self) {
        if let Err(e) = self.tab.close(false) {
            warn!("Failed to close tab: {e}");
        }
        let mut state = BROWSER_STATE.lock().unwrap();
        state.active_tabs = state.active_tabs.saturating_sub(1);
        state.last_activity = Instant::now();
    }
}

fn create_browser() -> Result<Browser> {
    // SAFETY: Remove LD_PRELOAD to bypass proxychains4 - Chrome's multi-process architecture
    // doesn't work with proxychains4's LD_PRELOAD interception.
    unsafe { std::env::remove_var("LD_PRELOAD") };

    let data_dir = chrome_data_dir();
    std::fs::create_dir_all(&data_dir)?;

    let launch_options = LaunchOptions {
        user_data_dir: Some(data_dir),
        headless: true,
        idle_browser_timeout: Duration::from_secs(600),
        ..LaunchOptions::default()
    };
    Ok(Browser::new(launch_options)?)
}

fn ensure_checker_thread(idle_timeout: Duration) {
    if CHECKER_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                let mut state = BROWSER_STATE.lock().unwrap();
                if state.browser.is_none() {
                    CHECKER_RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
                if state.active_tabs == 0 && state.last_activity.elapsed() >= idle_timeout {
                    state.browser.take();
                    CHECKER_RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
            }
        });
    }
}

/// Explicitly shut down the shared browser, killing the Chrome process.
/// Call this before process exit to prevent orphaned Chrome processes,
/// since Rust does not run destructors on statics.
pub fn shutdown_browser() {
    if let Ok(mut state) = BROWSER_STATE.lock() {
        state.browser.take();
    }
}

/// Open a new tab in the shared browser, navigate to the URL, and wait for load.
///
/// Returns a `TabGuard` that derefs to `Tab`. When the guard is dropped, the tab
/// is closed and the active tab count is decremented. If the browser has crashed,
/// it is automatically restarted.
pub fn open_tab(url: &str) -> Result<TabGuard> {
    let tab = {
        let mut state = BROWSER_STATE.lock().unwrap();

        // Ensure browser exists
        if state.browser.is_none() {
            state.browser = Some(create_browser()?);
        }

        // Try to open a new tab; if it fails, the browser may have crashed — restart once
        let tab = match state.browser.as_ref().unwrap().new_tab() {
            Ok(tab) => tab,
            Err(_) => {
                state.browser.take();
                state.browser = Some(create_browser()?);
                state.browser.as_ref().unwrap().new_tab()?
            }
        };

        state.active_tabs += 1;
        state.last_activity = Instant::now();
        ensure_checker_thread(state.idle_timeout);

        tab
        // Lock released here
    };

    // Navigation happens outside the lock so parallel tabs can be created
    tab.navigate_to(url)?;
    if let Err(e) = tab.wait_until_navigated() {
        warn!("wait_until_navigated failed: {e}");
    }
    if let Err(e) = tab.evaluate(WAIT_FOR_IDLE_JS, true) {
        warn!("wait-for-idle evaluation failed: {e}");
    }

    Ok(TabGuard { tab })
}
