use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use parking_lot::{Mutex, MutexGuard};
use serde::{Deserialize, Serialize};

/// Default `web_fetch` character allowance of a subagent tree root.
pub const DEFAULT_WEB_FETCH_CHAR_BUDGET: u32 = 400_000;

/// File name of a persisted budget node inside its owner's directory.
const BUDGET_FILE_NAME: &str = "web-fetch-budget.json";

/// A web_fetch character budget node, shared by a subagent tree.
pub struct WebFetchBudget {
    /// The conversation id of the subagent that created this node. Unique per
    /// session and used as the node id in the persisted file name.
    id: String,
    /// This node's own remaining allowance in chars.
    remaining: Mutex<u32>,
    /// Live parent, used for charging. `None` while an ancestor could not be
    /// loaded (or for a tree root).
    parent: Option<Arc<WebFetchBudget>>,
    /// Parent node id as it must be written to disk. Kept separately from
    /// `parent` so a node loaded without its ancestor does not erase the link.
    parent_id: Option<String>,
    /// `<owner state dir>/web-fetch-budget.json` when the tree is persisted.
    persist_path: Option<PathBuf>,
}

impl WebFetchBudget {
    /// New node (runtime creation). `remaining` is clamped to `parent.get()`
    /// when a parent is given, `parent_id` is taken from the parent, and the
    /// file is written immediately when `persist_path` is set.
    pub fn new(
        remaining: u32,
        id: String,
        parent: Option<&Arc<Self>>,
        persist_path: Option<PathBuf>,
    ) -> Arc<Self> {
        let parent_id = parent.map(|p| p.id.clone());
        let remaining = clamp_to_parent(remaining, parent);
        let node = Arc::new(Self {
            id,
            remaining: Mutex::new(remaining),
            parent: parent.cloned(),
            parent_id,
            persist_path,
        });
        node.persist(remaining);
        node
    }

    /// Restore from disk. Clamps to the live parent when one is given and does
    /// **not** write. The on-disk `parent_id` is kept even when `parent` could
    /// not be resolved, so a later resume can re-link the node.
    pub fn restore(
        remaining: u32,
        id: String,
        parent: Option<Arc<Self>>,
        parent_id: Option<String>,
        persist_path: Option<PathBuf>,
    ) -> Arc<Self> {
        let remaining = clamp_to_parent(remaining, parent.as_ref());
        Arc::new(Self {
            id,
            remaining: Mutex::new(remaining),
            parent,
            parent_id,
            persist_path,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Effective remaining chars: the minimum over this node's chain.
    pub fn get(&self) -> u32 {
        let chain = self.chain();
        let guards: Vec<MutexGuard<'_, u32>> =
            chain.iter().map(|node| node.remaining.lock()).collect();
        effective(&guards)
    }

    /// Atomically takes `min(chars, effective remaining)` from this node and
    /// every ancestor, persists every node it changed, and returns the amount
    /// actually taken.
    pub fn charge(&self, chars: u32) -> u32 {
        let chain = self.chain();
        let mut guards: Vec<MutexGuard<'_, u32>> =
            chain.iter().map(|node| node.remaining.lock()).collect();
        let amount = chars.min(effective(&guards));
        if amount == 0 {
            return 0;
        }
        for guard in &mut guards {
            **guard = (*guard).saturating_sub(amount);
        }
        // The guards are still held here on purpose: writing a node's file
        // before releasing its lock keeps a concurrent charge from persisting
        // an older value after a newer one.
        for (node, guard) in chain.iter().zip(guards.iter()) {
            node.persist(**guard);
        }
        amount
    }

    /// This node followed by its ancestors, in lock order.
    fn chain(&self) -> Vec<&Self> {
        let mut chain = vec![self];
        let mut next = self.parent.as_deref();
        while let Some(node) = next {
            chain.push(node);
            next = node.parent.as_deref();
        }
        chain
    }

    /// Best-effort persistence of `remaining`; failures are logged, never fatal.
    fn persist(&self, remaining: u32) {
        let Some(path) = self.persist_path.as_deref() else {
            return;
        };
        let file = WebFetchBudgetFile {
            parent: self.parent_id.clone(),
            remaining,
        };
        if let Err(err) = write_file(path, &file) {
            tracing::warn!("failed to write web fetch budget file {path:?}: {err:#}");
        }
    }
}

/// Clamp a node's own allowance to its parent's effective remaining.
fn clamp_to_parent(remaining: u32, parent: Option<&Arc<WebFetchBudget>>) -> u32 {
    match parent {
        Some(parent) => remaining.min(parent.get()),
        None => remaining,
    }
}

/// Minimum over a locked chain (the chain always contains at least one node).
fn effective(guards: &[MutexGuard<'_, u32>]) -> u32 {
    guards.iter().map(|guard| **guard).min().unwrap_or_default()
}

/// Atomically write `file` to `path` (`<path>.tmp` then rename).
fn write_file(path: &Path, file: &WebFetchBudgetFile) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("failed to create {dir:?}"))?;
    }
    let json =
        serde_json::to_string_pretty(file).context("failed to serialize web fetch budget")?;
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, json).with_context(|| format!("failed to write {tmp:?}"))?;
    std::fs::rename(&tmp, path).with_context(|| format!("failed to rename {tmp:?} -> {path:?}"))
}

/// On-disk shape of `web-fetch-budget.json`.
#[derive(Serialize, Deserialize)]
pub struct WebFetchBudgetFile {
    /// Parent node id (`None` for a tree root).
    #[serde(default)]
    pub parent: Option<String>,
    /// This node's own remaining chars.
    pub remaining: u32,
}

impl WebFetchBudgetFile {
    /// Reads and parses `<dir>/web-fetch-budget.json`.
    /// Returns `None` (and logs a warning) when the file is missing or invalid.
    pub fn read(dir: &Path) -> Option<Self> {
        let path = dir.join(BUDGET_FILE_NAME);
        let json = match std::fs::read_to_string(&path) {
            Ok(json) => json,
            Err(err) => {
                tracing::warn!("failed to read web fetch budget file {path:?}: {err}");
                return None;
            }
        };
        match serde_json::from_str(&json) {
            Ok(file) => Some(file),
            Err(err) => {
                tracing::warn!("invalid web fetch budget file {path:?}: {err}");
                None
            }
        }
    }
}
