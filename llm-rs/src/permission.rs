use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

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
    pub request_id: String,
    /// Path to a preview file on disk (e.g. for reviewing file-write content).
    /// The file extension is used for syntax highlighting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_file_path: Option<PathBuf>,
    /// When true, only "Allow once" and "Deny" should be offered — no
    /// session/project caching.  Used for complex bash commands.
    #[serde(default)]
    pub once_only: bool,
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
    waiters: HashMap<String, oneshot::Sender<bool>>,
    preview_file_path: Option<PathBuf>,
    once_only: bool,
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
    session_permissions: parking_lot::Mutex<HashSet<PermissionKey>>,
    project_permissions: parking_lot::Mutex<HashSet<PermissionKey>>,
    project_permissions_path: PathBuf,
    pending_requests: parking_lot::Mutex<HashMap<PermissionKey, PendingRequest>>,
}

impl PermissionManager {
    /// Create a new PermissionManager, loading project permissions from disk if they exist.
    pub fn new(project_path: PathBuf) -> Self {
        let project_permissions = Self::load_project_permissions(&project_path);
        PermissionManager {
            session_permissions: parking_lot::Mutex::new(HashSet::new()),
            project_permissions: parking_lot::Mutex::new(project_permissions),
            project_permissions_path: project_path,
            pending_requests: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    /// Check if a permission exists in session or project storage.
    pub fn has_permission(&self, tool: &str, key: &str, value: &str) -> bool {
        let pk = PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        };
        self.session_permissions.lock().contains(&pk)
            || self.project_permissions.lock().contains(&pk)
    }

    /// Register a pending permission request. If the same key is already pending,
    /// adds a new waiter to the existing entry (dedup). Returns a (request_id, receiver)
    /// pair. The request_id is a UUID identifying this specific invocation.
    pub fn register_request(
        &self,
        tool: &str,
        prompt: &str,
        key: &str,
        value: &str,
        preview_file_path: Option<PathBuf>,
        once_only: bool,
    ) -> (String, oneshot::Receiver<bool>) {
        let pk = PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        };
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending_requests.lock();
        if let Some(existing) = pending.get_mut(&pk) {
            existing.waiters.insert(request_id.clone(), tx);
        } else {
            let mut waiters = HashMap::new();
            waiters.insert(request_id.clone(), tx);
            pending.insert(
                pk,
                PendingRequest {
                    prompt: prompt.to_string(),
                    waiters,
                    preview_file_path,
                    once_only,
                },
            );
        }
        (request_id, rx)
    }

    /// Resolve a pending permission request. Sends the result to waiters and
    /// persists the decision to the appropriate storage.
    ///
    /// For `AllowOnce`, `request_id` must be provided to target a specific invocation.
    /// For `AllowSession`/`AllowProject`/`Deny`, all waiters are notified and
    /// `request_id` is ignored.
    pub fn resolve(
        &self,
        key: &PermissionKey,
        decision: &PermissionDecision,
        request_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // Save to storage based on decision
        match decision {
            PermissionDecision::AllowSession => {
                self.session_permissions.lock().insert(key.clone());
            }
            PermissionDecision::AllowProject => {
                self.project_permissions.lock().insert(key.clone());
                self.save_project_permissions()?;
            }
            PermissionDecision::AllowOnce | PermissionDecision::Deny => {}
        }

        // Notify waiters
        let mut pending = self.pending_requests.lock();
        if let Some(request) = pending.get_mut(key) {
            match (decision, request_id) {
                (PermissionDecision::AllowOnce, None) => {
                    anyhow::bail!(
                        "AllowOnce requires a request_id to target a specific invocation"
                    );
                }
                (PermissionDecision::AllowOnce, Some(rid)) => {
                    // AllowOnce: only approve the targeted waiter
                    if let Some(tx) = request.waiters.remove(rid)
                        && tx.send(true).is_err()
                    {
                        tracing::warn!("permission waiter dropped before AllowOnce was sent");
                    }
                    // Remove entry if no waiters left
                    if request.waiters.is_empty() {
                        pending.remove(key);
                    }
                }
                _ => {
                    // AllowSession/AllowProject/Deny: notify all and remove
                    let request = pending
                        .remove(key)
                        .expect("key must exist: verified via get_mut under same lock");
                    let allowed = !matches!(decision, PermissionDecision::Deny);
                    for (_, tx) in request.waiters {
                        if tx.send(allowed).is_err() {
                            tracing::warn!("permission waiter dropped before decision was sent");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Revoke a permission from both session and project storage.
    pub fn revoke(&self, key: &PermissionKey) -> anyhow::Result<()> {
        self.session_permissions.lock().remove(key);
        let removed = self.project_permissions.lock().remove(key);
        if removed {
            self.save_project_permissions()?;
        }
        Ok(())
    }

    /// Close all pending requests by sending `false` to all waiters.
    /// Used on session resume to clean up stale requests.
    pub fn close_all_pending(&self) {
        let mut pending = self.pending_requests.lock();
        for (_key, request) in pending.drain() {
            for (_, tx) in request.waiters {
                if tx.send(false).is_err() {
                    tracing::warn!("permission waiter dropped before close was sent");
                }
            }
        }
    }

    /// Get a full snapshot of the current permission state.
    pub fn snapshot(&self) -> PermissionState {
        let pending = self.pending_requests.lock();
        let pending_infos: Vec<PendingPermissionInfo> = pending
            .iter()
            .map(|(k, r)| {
                // Use the first waiter's request_id (arbitrary but stable for display)
                let request_id = r.waiters.keys().next().cloned().unwrap_or_default();
                PendingPermissionInfo {
                    tool: k.tool.clone(),
                    prompt: r.prompt.clone(),
                    key: k.key.clone(),
                    value: k.value.clone(),
                    request_id,
                    preview_file_path: r.preview_file_path.clone(),
                    once_only: r.once_only,
                }
            })
            .collect();

        let session: Vec<PermissionKey> = self.session_permissions.lock().iter().cloned().collect();
        let project: Vec<PermissionKey> = self.project_permissions.lock().iter().cloned().collect();

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
        let perms: Vec<PermissionKey> = self.project_permissions.lock().iter().cloned().collect();
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
    /// Callback invoked after a permission request is approved.
    on_approved_fn: Arc<dyn Fn() + Send + Sync>,
    /// Tracks whether the user denied permission during this tool execution.
    denied: Arc<AtomicBool>,
    /// True while waiting for user approval. Used by `TimeoutStream` to
    /// pause the deadline so approval wait time doesn't count as timeout.
    approval_pending: Arc<AtomicBool>,
    /// Session directory for writing preview files.
    session_dir: Option<PathBuf>,
}

impl ScopedPermissionManager {
    /// Create a scoped permission manager for a specific tool.
    pub fn new(
        tool_name: &str,
        manager: Arc<PermissionManager>,
        notify_fn: Arc<dyn Fn() + Send + Sync>,
        on_approved_fn: Arc<dyn Fn() + Send + Sync>,
        session_dir: Option<PathBuf>,
    ) -> Self {
        ScopedPermissionManager {
            tool_name: tool_name.to_string(),
            manager,
            notify_fn,
            on_approved_fn,
            denied: Arc::new(AtomicBool::new(false)),
            approval_pending: Arc::new(AtomicBool::new(false)),
            session_dir,
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
            on_approved_fn: Arc::new(|| {}),
            denied: Arc::new(AtomicBool::new(false)),
            approval_pending: Arc::new(AtomicBool::new(false)),
            session_dir: None,
        }
    }

    /// Check if a permission exists without prompting.
    pub fn has_permission(&self, key: &str, value: &str) -> bool {
        self.has_permission_for(&self.tool_name.clone(), key, value)
    }

    /// Check if the action is permitted. If no saved preference exists, registers
    /// a pending request, notifies the UI, and awaits the user's decision.
    pub async fn ask_permission(&self, prompt: &str, key: &str, value: &str) -> anyhow::Result<()> {
        self.ask_permission_for(&self.tool_name.clone(), prompt, key, value)
            .await
    }

    /// Check if a permission exists without prompting, using a custom scope
    /// instead of this manager's tool name. Useful for shared permission scopes
    /// (e.g. `"file_read"`) that span multiple tools.
    pub fn has_permission_for(&self, scope: &str, key: &str, value: &str) -> bool {
        self.manager.has_permission(scope, key, value)
    }

    /// Check if the action is permitted using a custom scope instead of this
    /// manager's tool name. If no saved preference exists, registers a pending
    /// request, notifies the UI, and awaits the user's decision.
    pub async fn ask_permission_for(
        &self,
        scope: &str,
        prompt: &str,
        key: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        self.ask_permission_inner(scope, prompt, key, value, None)
            .await
    }

    /// Like `ask_permission_with_preview`, but writes `content` to a preview file under
    /// `session_dir/tool-file-preview/` so the UI can offer "[v] View in nvim".
    /// `file_extension` is used for syntax highlighting (e.g. "rs", "py").
    pub async fn ask_permission_with_preview(
        &self,
        scope: &str,
        prompt: &str,
        key: &str,
        value: &str,
        content: &str,
        file_extension: &str,
    ) -> anyhow::Result<()> {
        let preview_path = match self.write_preview_file(content, file_extension) {
            Ok(path) => Some(path),
            Err(e) => {
                tracing::warn!("Failed to write preview file: {}", e);
                None
            }
        };
        self.ask_permission_inner(scope, prompt, key, value, preview_path)
            .await
    }

    /// Always prompt the user and never cache the result. The approval UI will
    /// only offer "Allow once" and "Deny". Used for complex bash commands where
    /// caching a permission prefix is inherently unsafe.
    pub async fn ask_permission_once(
        &self,
        scope: &str,
        prompt: &str,
        content: &str,
        file_extension: &str,
    ) -> anyhow::Result<()> {
        let preview_path = match self.write_preview_file(content, file_extension) {
            Ok(path) => Some(path),
            Err(e) => {
                tracing::warn!("Failed to write preview file: {}", e);
                None
            }
        };

        // Use the full command as key+value so each complex command gets its own
        // pending entry, but we never check or store cached permissions.
        let key = "command";
        let value = content;

        let (_request_id, rx) =
            self.manager
                .register_request(scope, prompt, key, value, preview_path.clone(), true);

        // Notify UI that permission state changed (idempotent)
        (self.notify_fn)();

        self.approval_pending.store(true, Ordering::Release);
        let allowed = rx.await.unwrap_or(false);
        self.approval_pending.store(false, Ordering::Release);

        Self::cleanup_preview_file(&preview_path);

        if allowed {
            (self.on_approved_fn)();
            Ok(())
        } else {
            self.denied.store(true, Ordering::Relaxed);
            Err(anyhow!(
                "Permission denied: {} The user chose not to allow this action.",
                prompt
            ))
        }
    }

    /// Core permission-request flow shared by `ask_permission_for` and
    /// `ask_permission_with_preview`. Cleans up the preview file (if any)
    /// after the decision is received.
    async fn ask_permission_inner(
        &self,
        scope: &str,
        prompt: &str,
        key: &str,
        value: &str,
        preview_file_path: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        if self.manager.has_permission(scope, key, value) {
            Self::cleanup_preview_file(&preview_file_path);
            return Ok(());
        }

        let (_request_id, rx) = self.manager.register_request(
            scope,
            prompt,
            key,
            value,
            preview_file_path.clone(),
            false,
        );

        // Notify UI that permission state changed (idempotent)
        (self.notify_fn)();

        self.approval_pending.store(true, Ordering::Release);
        let allowed = rx.await.unwrap_or(false);
        self.approval_pending.store(false, Ordering::Release);

        Self::cleanup_preview_file(&preview_file_path);

        if allowed {
            (self.on_approved_fn)();
            Ok(())
        } else {
            self.denied.store(true, Ordering::Relaxed);
            Err(anyhow!(
                "Permission denied: {} The user chose not to allow this action.",
                prompt
            ))
        }
    }

    /// Write content to a preview file under session_dir/tool-file-preview/.
    fn write_preview_file(&self, content: &str, extension: &str) -> anyhow::Result<PathBuf> {
        let session_dir = self
            .session_dir
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No session_dir configured for preview files"))?;
        let preview_dir = session_dir.join("tool-file-preview");
        std::fs::create_dir_all(&preview_dir)?;
        let filename = format!("{}.{}", Uuid::new_v4(), extension);
        let path = preview_dir.join(filename);
        std::fs::write(&path, content)?;
        Ok(path)
    }

    /// Remove a preview file if it exists.
    fn cleanup_preview_file(path: &Option<PathBuf>) {
        if let Some(p) = path
            && let Err(e) = std::fs::remove_file(p)
        {
            tracing::warn!("Failed to clean up preview file {}: {}", p.display(), e);
        }
    }

    /// Returns the session directory, if configured.
    pub fn session_dir(&self) -> Option<&std::path::Path> {
        self.session_dir.as_deref()
    }

    /// Returns true if the user denied permission during this tool execution.
    pub fn was_denied(&self) -> bool {
        self.denied.load(Ordering::Relaxed)
    }

    /// Returns a shared handle to the approval-pending flag.
    /// Used by `TimeoutStream` to pause the deadline while waiting for user approval.
    pub fn approval_pending(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.approval_pending)
    }
}
