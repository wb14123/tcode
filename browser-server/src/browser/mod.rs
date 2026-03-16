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

    let lock_file = data_dir.join("SingletonLock");
    if lock_file.exists() {
        anyhow::bail!(
            "Chrome profile is already locked by another process ({}). \
             Stop the other browser-server instance or remove the lock file, then retry.",
            lock_file.display()
        );
    }

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

/// Launch a visible (non-headless) Chrome with the persistent profile for the user
/// to log in to accounts. Blocks until the browser window is closed.
pub async fn launch_interactive() -> Result<()> {
    let data_dir = chrome_data_dir();
    std::fs::create_dir_all(&data_dir)?;

    let lock_file = data_dir.join("SingletonLock");
    if lock_file.exists() {
        anyhow::bail!(
            "Chrome profile is already locked ({}). \
             Stop the running browser-server first, then retry.",
            lock_file.display()
        );
    }

    println!(
        "Launching Chrome with persistent profile at: {}",
        data_dir.display()
    );
    println!("Log in to your accounts, then close the browser window to save the session.");
    println!();

    let launch_options = LaunchOptions {
        headless: false,
        user_data_dir: Some(data_dir),
        ..LaunchOptions::default()
    };

    let browser = Browser::new(launch_options)?;

    if let Some(pid) = browser.get_process_id() {
        loop {
            if !std::path::Path::new(&format!("/proc/{}", pid)).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    println!("Browser closed. Your session data has been saved.");
    Ok(())
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

    // Override user-agent to avoid being blocked by sites that reject HeadlessChrome
    tracing::info!("open_tab: setting user-agent");
    tab.set_user_agent(
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        None,
        None,
    )?;

    // Navigation happens outside the lock so parallel tabs can be created
    tracing::info!("open_tab: calling navigate_to");
    tab.navigate_to(url)?;
    tracing::info!("open_tab: calling wait_until_navigated");
    tab.wait_until_navigated()?;
    tracing::info!("open_tab: calling wait-for-idle");
    tab.evaluate(WAIT_FOR_IDLE_JS, true)?;
    tracing::info!("open_tab: navigation complete");

    Ok(TabGuard { tab })
}
