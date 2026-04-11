use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// --- Scope name constants ---
pub const SCOPE_FILE_READ: &str = "file_read";
pub const SCOPE_FILE_WRITE: &str = "file_write";
pub const SCOPE_BASH: &str = "bash";
pub const SCOPE_WEB_FETCH: &str = "web_fetch";

// --- Key name constants ---
pub const KEY_PATH: &str = "path";
pub const KEY_COMMAND: &str = "command";
pub const KEY_HOSTNAME: &str = "hostname";

/// Reserved value meaning "match any value under this (tool, key)".
/// Tools must NEVER pass this as a literal value to `ask_permission*`.
/// The permission manager treats a stored entry with this value as a
/// wildcard that matches every value for the same (tool, key) pair.
/// The wildcard can only enter the permission store via the user-initiated
/// add-permission UI flow (`ClientMessage::AddPermission`).
pub const WILDCARD_VALUE: &str = "*";

/// Registry of all known permission scopes and their associated key names.
///
/// When implementing a new tool that requires permissions, you MUST:
/// 1. Add a `SCOPE_*` constant for your tool's scope name above.
/// 2. Add `KEY_*` constants for any new key names (reuse existing ones if applicable).
/// 3. Add an entry to this array mapping your scope to its keys.
///
/// This registry is used by the permission tree UI to display all available
/// scopes and keys, even before any permissions have been requested.
pub const ALL_SCOPES: &[(&str, &[&str])] = &[
    (SCOPE_BASH, &[KEY_COMMAND]),
    (SCOPE_FILE_READ, &[KEY_PATH]),
    (SCOPE_FILE_WRITE, &[KEY_PATH]),
    (SCOPE_WEB_FETCH, &[KEY_HOSTNAME]),
];

/// RAII guard that removes a waiter from `PermissionManager::pending_requests`
/// when dropped. This ensures cleanup even if the permission-wait future is
/// abandoned (e.g. by `CancellableStream` intercepting the cancel token at the
/// outer poll level before the inner `select!` can run its cancel arm).
/// Call `defuse()` on normal completion to prevent cleanup.
struct PendingWaiterGuard {
    manager: Arc<PermissionManager>,
    tool: String,
    key: String,
    value: String,
    request_id: String,
    defused: bool,
}

impl PendingWaiterGuard {
    fn new(
        manager: Arc<PermissionManager>,
        tool: &str,
        key: &str,
        value: &str,
        request_id: &str,
    ) -> Self {
        Self {
            manager,
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
            request_id: request_id.to_string(),
            defused: false,
        }
    }

    /// Prevent the guard from removing the waiter on drop.
    /// Called when the permission request completes normally (approved or denied).
    fn defuse(&mut self) {
        self.defused = true;
    }
}

impl Drop for PendingWaiterGuard {
    fn drop(&mut self) {
        if !self.defused {
            self.manager
                .remove_waiter(&self.tool, &self.key, &self.value, &self.request_id);
        }
    }
}

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
    Deny { reason: Option<String> },
}

/// Internal payload delivered to pending permission waiters when a request
/// is resolved. Allow* decisions deliver `Allowed`; `Deny { reason }` delivers
/// `Denied(reason.clone())`. Shutdown / external-cancel paths (e.g.
/// `close_all_pending`) deliver `Cancelled` — semantically distinct from a
/// user denial, so the scoped manager does not mark itself `denied`.
#[derive(Debug, Clone)]
pub(crate) enum ResolveOutcome {
    Allowed,
    Denied(Option<String>),
    Cancelled,
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
    waiters: HashMap<String, oneshot::Sender<ResolveOutcome>>,
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
    ///
    /// Returns `true` if either the exact `(tool, key, value)` triple is
    /// stored, or a wildcard entry `(tool, key, WILDCARD_VALUE)` is stored
    /// for the same `(tool, key)` pair. See [`WILDCARD_VALUE`].
    pub fn has_permission(&self, tool: &str, key: &str, value: &str) -> bool {
        let pk = PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        };
        let wildcard_pk = PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: WILDCARD_VALUE.to_string(),
        };
        let session = self.session_permissions.lock();
        if session.contains(&pk) || session.contains(&wildcard_pk) {
            return true;
        }
        drop(session);
        let project = self.project_permissions.lock();
        project.contains(&pk) || project.contains(&wildcard_pk)
    }

    /// Register a pending permission request. If the same key is already pending,
    /// adds a new waiter to the existing entry (dedup). Returns a (request_id, receiver)
    /// pair. The request_id is a UUID identifying this specific invocation.
    pub(crate) fn register_request(
        &self,
        tool: &str,
        prompt: &str,
        key: &str,
        value: &str,
        preview_file_path: Option<PathBuf>,
        once_only: bool,
    ) -> (String, oneshot::Receiver<ResolveOutcome>) {
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
        let mut pending = self.pending_requests.lock();

        // Enforce once_only: reject AllowSession/AllowProject when the pending
        // request was marked once_only (only AllowOnce and Deny are valid).
        if let Some(request) = pending.get(key)
            && request.once_only
            && matches!(
                decision,
                PermissionDecision::AllowSession | PermissionDecision::AllowProject
            )
        {
            anyhow::bail!(
                "cannot use {decision:?} for a once_only permission request; \
                 only AllowOnce or Deny are permitted"
            );
        }

        // Save to storage based on decision (independent of pending state)
        match decision {
            PermissionDecision::AllowSession => {
                self.session_permissions.lock().insert(key.clone());
            }
            PermissionDecision::AllowProject => {
                self.project_permissions.lock().insert(key.clone());
                self.save_project_permissions()?;
            }
            PermissionDecision::AllowOnce | PermissionDecision::Deny { .. } => {}
        }

        // Notify waiters if there's a pending request
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
                        && tx.send(ResolveOutcome::Allowed).is_err()
                    {
                        tracing::warn!("permission waiter dropped before AllowOnce was sent");
                    }
                    // Remove entry if no waiters left
                    if request.waiters.is_empty() {
                        pending.remove(key);
                    }
                }
                (PermissionDecision::AllowSession | PermissionDecision::AllowProject, _) => {
                    // Notify all waiters with Allowed and remove the entry.
                    let request = pending
                        .remove(key)
                        .expect("key must exist: verified via get_mut under same lock");
                    for (_, tx) in request.waiters {
                        if tx.send(ResolveOutcome::Allowed).is_err() {
                            tracing::warn!("permission waiter dropped before decision was sent");
                        }
                    }
                }
                (PermissionDecision::Deny { reason }, _) => {
                    // Notify all waiters with Denied(reason.clone()) and remove the entry.
                    let request = pending
                        .remove(key)
                        .expect("key must exist: verified via get_mut under same lock");
                    for (_, tx) in request.waiters {
                        if tx.send(ResolveOutcome::Denied(reason.clone())).is_err() {
                            tracing::warn!("permission waiter dropped before decision was sent");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Add a permission directly (user-initiated, bypasses pending request flow).
    /// Only `Session` and `Project` scopes are valid. `Once` is silently ignored.
    pub fn add_permission(&self, key: PermissionKey, scope: PermissionScope) -> anyhow::Result<()> {
        match scope {
            PermissionScope::Session => {
                self.session_permissions.lock().insert(key);
            }
            PermissionScope::Project => {
                self.project_permissions.lock().insert(key);
                self.save_project_permissions()?;
            }
            PermissionScope::Once => {
                // Once is not valid for user-initiated adds; silently ignore
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

    /// Close all pending requests by delivering `Cancelled` to all waiters.
    /// Used on session resume and conversation/tool cancellation to clean up
    /// stale requests. Waiters do NOT get marked as denied — the scoped
    /// manager distinguishes cancel from denial via `was_denied()`.
    pub fn close_all_pending(&self) {
        let mut pending = self.pending_requests.lock();
        for (_key, request) in pending.drain() {
            for (_, tx) in request.waiters {
                if tx.send(ResolveOutcome::Cancelled).is_err() {
                    tracing::warn!("permission waiter dropped before close was sent");
                }
            }
        }
    }

    /// Remove a specific waiter from a pending request. If no waiters remain,
    /// removes the entire pending entry. Used when a tool is cancelled while
    /// waiting for permission.
    pub fn remove_waiter(&self, tool: &str, key: &str, value: &str, request_id: &str) {
        let pk = PermissionKey {
            tool: tool.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        };
        let mut pending = self.pending_requests.lock();
        let should_remove = if let Some(entry) = pending.get_mut(&pk) {
            entry.waiters.remove(request_id);
            entry.waiters.is_empty()
        } else {
            false
        };
        if should_remove {
            pending.remove(&pk);
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
    /// Optional cancellation token to make permission waits cancel-aware.
    cancel_token: Option<CancellationToken>,
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
            cancel_token: None,
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
            cancel_token: None,
        }
    }

    /// Set the cancellation token so permission waits can be interrupted on cancel.
    pub fn set_cancel_token(&mut self, token: CancellationToken) {
        self.cancel_token = Some(token);
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
    ///
    /// **Important:** Any new scope/key combination passed here must be
    /// registered in [`ALL_SCOPES`] so the permission tree UI can display it.
    pub async fn ask_permission_for(
        &self,
        scope: &str,
        prompt: &str,
        key: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        self.ask_permission_inner(scope, prompt, key, value, None, false)
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
        self.ask_permission_inner(scope, prompt, key, value, preview_path, false)
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
        self.ask_permission_inner(scope, prompt, "command", content, preview_path, true)
            .await
    }

    /// Core permission-request flow shared by `ask_permission_for`,
    /// `ask_permission_with_preview`, and `ask_permission_once`.
    /// Cleans up the preview file (if any) after the decision is received.
    /// When `once_only` is true, skips the cached-permission check and marks
    /// the request so the approval UI only offers "Allow once" / "Deny".
    async fn ask_permission_inner(
        &self,
        scope: &str,
        prompt: &str,
        key: &str,
        value: &str,
        preview_file_path: Option<PathBuf>,
        once_only: bool,
    ) -> anyhow::Result<()> {
        // Hard-fail if a tool attempts to use the reserved wildcard value.
        // This prevents a buggy tool from ever storing `"*"` via the pending
        // request flow (e.g. via AllowSession/AllowProject resolution). The
        // wildcard can only enter the permission store through the
        // user-initiated add-permission UI flow.
        if value == WILDCARD_VALUE {
            Self::cleanup_preview_file(&preview_file_path);
            return Err(anyhow!(
                "internal error: tool attempted to use reserved wildcard value '*' \
                 for scope={scope:?} key={key:?}; this is a bug — the wildcard can \
                 only be created via the user-initiated add-permission UI flow"
            ));
        }

        if !once_only && self.manager.has_permission(scope, key, value) {
            Self::cleanup_preview_file(&preview_file_path);
            return Ok(());
        }

        let (request_id, rx) = self.manager.register_request(
            scope,
            prompt,
            key,
            value,
            preview_file_path.clone(),
            once_only,
        );

        // Guard ensures the waiter is removed from pending_requests if this
        // future is dropped (e.g. CancellableStream intercepting the cancel).
        let mut guard =
            PendingWaiterGuard::new(Arc::clone(&self.manager), scope, key, value, &request_id);

        // If already cancelled, clean up immediately without notifying the UI
        if self.cancel_token.as_ref().is_some_and(|t| t.is_cancelled()) {
            // guard drops here and calls remove_waiter
            Self::cleanup_preview_file(&preview_file_path);
            return Err(anyhow!("Tool cancelled while waiting for permission"));
        }

        // Notify UI that permission state changed (idempotent)
        (self.notify_fn)();

        self.approval_pending.store(true, Ordering::Release);

        let outcome: ResolveOutcome = if let Some(token) = &self.cancel_token {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    // guard drops here and calls remove_waiter
                    self.approval_pending.store(false, Ordering::Release);
                    Self::cleanup_preview_file(&preview_file_path);
                    // Return error without setting `denied` — this is a cancellation, not a denial.
                    // The caller (execute_regular_tool) sends PermissionUpdated to refresh the UI.
                    return Err(anyhow!("Tool cancelled while waiting for permission"));
                }
                result = rx => result.unwrap_or(ResolveOutcome::Cancelled),
            }
        } else {
            rx.await.unwrap_or(ResolveOutcome::Cancelled)
        };

        // Normal completion — defuse the guard so it doesn't remove the waiter
        // (the resolve() call already handled it).
        guard.defuse();

        self.approval_pending.store(false, Ordering::Release);

        Self::cleanup_preview_file(&preview_file_path);

        match outcome {
            ResolveOutcome::Allowed => {
                (self.on_approved_fn)();
                Ok(())
            }
            ResolveOutcome::Denied(reason) => {
                self.denied.store(true, Ordering::Relaxed);
                // Sanitize the reason: trim, then collapse any run of
                // whitespace (spaces, tabs, newlines) to a single space.
                // The UI caps reason length and submits on Enter, but
                // `reason` is wire-protocol-shaped and an in-process or
                // socket-level caller could technically send arbitrary bytes,
                // so collapse here so a multi-line reason can never break
                // the single-line display layout downstream.
                let sanitized = reason
                    .as_deref()
                    .map(|r| {
                        let mut out = String::with_capacity(r.len());
                        for word in r.split_whitespace() {
                            if !out.is_empty() {
                                out.push(' ');
                            }
                            out.push_str(word);
                        }
                        out
                    })
                    .filter(|s| !s.is_empty());
                match sanitized {
                    Some(r) => Err(anyhow!(
                        "Permission denied: {} The user chose not to allow this action. \
                         The user's reason: {}",
                        prompt,
                        r
                    )),
                    None => Err(anyhow!(
                        "Permission denied: {} The user chose not to allow this action.",
                        prompt
                    )),
                }
            }
            ResolveOutcome::Cancelled => {
                // Shutdown / external-cancel path (close_all_pending, or a
                // dropped sender). Do NOT touch `denied` — `was_denied()`
                // must stay false so the caller classifies this as a cancel,
                // not a user denial.
                Err(anyhow!("Tool cancelled while waiting for permission"))
            }
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
