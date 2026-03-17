use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// Scope at which a permission was granted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionScope {
    Once,
    Session,
    Project,
}

/// User decision when resolving a permission request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionDecision {
    AllowOnce,
    AllowSession,
    AllowProject,
    Deny,
}

/// Unique key identifying a permission: (tool, key, value).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PermissionKey {
    pub tool: String,
    pub key: String,
    pub value: String,
}

/// Information about a pending permission request, for UI display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPermissionInfo {
    pub tool: String,
    pub prompt: String,
    pub key: String,
    pub value: String,
}

/// Full snapshot of permission state, sent to UI on query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionState {
    pub pending: Vec<PendingPermissionInfo>,
    pub session: Vec<PermissionKey>,
    pub project: Vec<PermissionKey>,
}

/// Internal tracking for a pending permission request with multiple waiters.
struct PendingRequest {
    prompt: String,
    waiters: Vec<oneshot::Sender<bool>>,
}

/// On-disk format for project-level permissions.
#[derive(Debug, Serialize, Deserialize)]
struct ProjectPermissionsFile {
    version: u32,
    permissions: Vec<PermissionKey>,
}

/// Pure-state permission manager. Does not emit events — callers (ConversationClient)
/// handle event broadcasting.
pub struct PermissionManager {
    session_permissions: std::sync::Mutex<HashSet<PermissionKey>>,
    project_permissions: std::sync::Mutex<HashSet<PermissionKey>>,
    project_permissions_path: PathBuf,
    pending_requests: std::sync::Mutex<HashMap<PermissionKey, PendingRequest>>,
}

impl PermissionManager {
    /// Create a new PermissionManager, loading project permissions from disk if they exist.
    pub fn new(project_path: PathBuf) -> Self {
        let project_permissions = Self::load_project_permissions(&project_path);
        PermissionManager {
            session_permissions: std::sync::Mutex::new(HashSet::new()),
            project_permissions: std::sync::Mutex::new(project_permissions),
            project_permissions_path: project_path,
            pending_requests: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Check if a permission exists in session or project storage.
    pub fn has_permission(&self, tool: &str, key: &str, value: &str) -> bool {
        let pk = PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        };
        self.session_permissions.lock().unwrap().contains(&pk)
            || self.project_permissions.lock().unwrap().contains(&pk)
    }

    /// Register a pending permission request. If the same key is already pending,
    /// adds a new waiter to the existing entry (dedup). Returns a receiver that
    /// will resolve to `true` (allowed) or `false` (denied).
    pub fn register_request(
        &self,
        tool: &str,
        prompt: &str,
        key: &str,
        value: &str,
    ) -> oneshot::Receiver<bool> {
        let pk = PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        };
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending_requests.lock().unwrap();
        if let Some(existing) = pending.get_mut(&pk) {
            existing.waiters.push(tx);
        } else {
            pending.insert(pk, PendingRequest {
                prompt: prompt.to_string(),
                waiters: vec![tx],
            });
        }
        rx
    }

    /// Resolve a pending permission request. Sends the result to all waiters and
    /// persists the decision to the appropriate storage.
    pub fn resolve(&self, key: &PermissionKey, decision: &PermissionDecision) -> anyhow::Result<()> {
        let allowed = matches!(
            decision,
            PermissionDecision::AllowOnce | PermissionDecision::AllowSession | PermissionDecision::AllowProject
        );

        // Save to storage based on decision
        match decision {
            PermissionDecision::AllowSession => {
                self.session_permissions.lock().unwrap().insert(key.clone());
            }
            PermissionDecision::AllowProject => {
                self.project_permissions.lock().unwrap().insert(key.clone());
                self.save_project_permissions()?;
            }
            PermissionDecision::AllowOnce | PermissionDecision::Deny => {}
        }

        // Notify all waiters
        let mut pending = self.pending_requests.lock().unwrap();
        if let Some(request) = pending.remove(key) {
            for tx in request.waiters {
                // Ignore send errors — receiver may have been dropped
                let _ = tx.send(allowed);
            }
        }

        Ok(())
    }

    /// Revoke a permission from both session and project storage.
    pub fn revoke(&self, key: &PermissionKey) -> anyhow::Result<()> {
        self.session_permissions.lock().unwrap().remove(key);
        let removed = self.project_permissions.lock().unwrap().remove(key);
        if removed {
            self.save_project_permissions()?;
        }
        Ok(())
    }

    /// Close all pending requests by sending `false` to all waiters.
    /// Used on session resume to clean up stale requests.
    pub fn close_all_pending(&self) {
        let mut pending = self.pending_requests.lock().unwrap();
        for (_key, request) in pending.drain() {
            for tx in request.waiters {
                let _ = tx.send(false);
            }
        }
    }

    /// Get a full snapshot of the current permission state.
    pub fn snapshot(&self) -> PermissionState {
        let pending = self.pending_requests.lock().unwrap();
        let pending_infos: Vec<PendingPermissionInfo> = pending
            .iter()
            .map(|(k, r)| PendingPermissionInfo {
                tool: k.tool.clone(),
                prompt: r.prompt.clone(),
                key: k.key.clone(),
                value: k.value.clone(),
            })
            .collect();

        let session: Vec<PermissionKey> = self.session_permissions.lock().unwrap()
            .iter().cloned().collect();
        let project: Vec<PermissionKey> = self.project_permissions.lock().unwrap()
            .iter().cloned().collect();

        PermissionState {
            pending: pending_infos,
            session,
            project,
        }
    }

    fn load_project_permissions(path: &PathBuf) -> HashSet<PermissionKey> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return HashSet::new();
        };
        let Ok(file) = serde_json::from_str::<ProjectPermissionsFile>(&content) else {
            return HashSet::new();
        };
        file.permissions.into_iter().collect()
    }

    fn save_project_permissions(&self) -> anyhow::Result<()> {
        let perms: Vec<PermissionKey> = self.project_permissions.lock().unwrap()
            .iter().cloned().collect();
        let file = ProjectPermissionsFile {
            version: 1,
            permissions: perms,
        };
        let json = serde_json::to_string_pretty(&file)?;

        // Ensure parent directory exists
        if let Some(parent) = self.project_permissions_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&self.project_permissions_path, json)?;
        Ok(())
    }
}

/// Scoped permission handle for a specific tool. Passed via `ToolContext`.
/// Handles the ask-permission flow: check stored permissions, register request,
/// notify UI, and await result.
#[derive(Clone)]
pub struct ScopedPermissionManager {
    tool_name: String,
    manager: Arc<PermissionManager>,
    /// Callback to notify the UI that permission state changed.
    /// The ConversationClient provides this via a closure.
    notify_fn: Arc<dyn Fn() + Send + Sync>,
}

impl ScopedPermissionManager {
    /// Create a scoped permission manager for a specific tool.
    pub fn new(
        tool_name: &str,
        manager: Arc<PermissionManager>,
        notify_fn: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        ScopedPermissionManager {
            tool_name: tool_name.to_string(),
            manager,
            notify_fn,
        }
    }

    /// Create a scoped permission manager backed by a temporary `PermissionManager`
    /// and a no-op notify callback. All permission checks pass because no permissions
    /// are ever requested. Useful for tests and examples that execute tools without
    /// a full conversation/UI setup.
    pub fn always_allow(tool_name: &str) -> Self {
        let manager = Arc::new(PermissionManager::new(
            std::env::temp_dir().join(format!("llm-rs-test-pm-{}.json", std::process::id())),
        ));
        ScopedPermissionManager {
            tool_name: tool_name.to_string(),
            manager,
            notify_fn: Arc::new(|| {}),
        }
    }

    /// Check if a permission exists without prompting.
    pub fn has_permission(&self, key: &str, value: &str) -> bool {
        self.manager.has_permission(&self.tool_name, key, value)
    }

    /// Check if the action is permitted. If no saved preference exists, registers
    /// a pending request, notifies the UI, and awaits the user's decision.
    pub async fn ask_permission(&self, prompt: &str, key: &str, value: &str) -> bool {
        if self.manager.has_permission(&self.tool_name, key, value) {
            return true;
        }

        let rx = self.manager.register_request(&self.tool_name, prompt, key, value);

        // Notify UI that permission state changed (idempotent)
        (self.notify_fn)();

        rx.await.unwrap_or(false)
    }
}
