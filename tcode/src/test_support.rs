//! Shared utilities for tcode tests.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing::field::Visit;
use tracing::{Event, Level, Metadata, Subscriber};

/// Process-wide lock serializing tests that mutate the `HOME` env var.
///
/// `dirs::home_dir()` reads `$HOME` on every call, and tests within one
/// binary run in parallel threads, so any test that redirects `HOME` must
/// hold this lock for its whole duration to keep concurrent tests from ever
/// observing a foreign home directory.
pub fn home_env_lock() -> &'static parking_lot::Mutex<()> {
    static LOCK: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| parking_lot::Mutex::new(()))
}

/// RAII guard that redirects `HOME` to a test directory for the duration of
/// the test and restores the previous value on drop.
pub struct HomeGuard {
    _guard: parking_lot::MutexGuard<'static, ()>,
    previous_home: Option<OsString>,
}

impl HomeGuard {
    pub fn set(home_dir: &Path) -> Self {
        let guard = home_env_lock().lock();
        let previous_home = std::env::var_os("HOME");
        // SAFETY: the process-wide lock ensures no concurrent test observes
        // a half-mutated HOME.
        unsafe { std::env::set_var("HOME", home_dir) };
        Self {
            _guard: guard,
            previous_home,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(previous_home) => {
                // SAFETY: restoration happens while the same process-wide lock is held.
                unsafe { std::env::set_var("HOME", previous_home) };
            }
            None => {
                // SAFETY: restoration happens while the same process-wide lock is held.
                unsafe { std::env::remove_var("HOME") };
            }
        }
    }
}

/// Minimal tracing subscriber that records the messages of WARN-level events,
/// used to assert that a code path logs as expected.
#[derive(Default)]
pub struct WarnCapture {
    messages: parking_lot::Mutex<Vec<String>>,
}

impl Subscriber for WarnCapture {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &Event<'_>) {
        if *event.metadata().level() >= Level::WARN {
            let mut visitor = MessageVisitor(Vec::new());
            event.record(&mut visitor);
            self.messages.lock().push(visitor.0.join(" "));
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

impl WarnCapture {
    /// The WARN-level event messages recorded so far.
    pub fn messages(&self) -> Vec<String> {
        self.messages.lock().clone()
    }
}

struct MessageVisitor(Vec<String>);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={:?}", field.name(), value));
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push(format!("{}={}", field.name(), value));
    }
}

/// Per-test temp dir under the workspace target dir; removed on drop
/// (cleanup runs on success and on panic).
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(module: &str) -> Self {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../target/test-tmp/{module}"));
        std::fs::create_dir_all(&root).expect("failed to create test root");
        let dir = root.join(uuid::Uuid::new_v4().to_string());
        // Cleanup before: remove any stale leftover at this exact path.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TestDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
