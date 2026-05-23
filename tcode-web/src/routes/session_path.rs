use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tcode_runtime::session::{
    Session, generate_session_id, list_sessions_at, read_session_mode, validate_session_id,
};

#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

pub(crate) type ApiResult<T> = Result<T, ApiError>;

/// The authenticated user's session root — injected by the auth middleware
/// via `axum::Extension`. Carries the canonicalized `session_dir` path
/// from `web-users.toml`.
#[derive(Clone)]
pub(crate) struct SessionRoot {
    pub(crate) path: PathBuf,
}

impl SessionRoot {
    /// Validate `id`, join to root, verify the directory exists, canonicalize,
    /// containment-check against root, and return a `SessionDir`.
    pub(crate) fn open_session(&self, id: &str) -> ApiResult<SessionDir> {
        validate_session_id(id).map_err(|e| ApiError::bad_request(e.to_string()))?;
        let path = self.path.join(id);
        if !path.is_dir() {
            return Err(ApiError::not_found("session not found"));
        }
        let canonical = path
            .canonicalize()
            .map_err(|e| ApiError::internal(e.to_string()))?;
        if !canonical.starts_with(&self.path) {
            return Err(ApiError::bad_request("path traversal detected"));
        }
        // Reject non-WebOnly sessions — the web UI must not serve Normal-mode
        // sessions created via TUI. 404 to avoid leaking session existence.
        let mode = read_session_mode(&canonical).map_err(|e| ApiError::internal(e.to_string()))?;
        if !mode.is_web_only() {
            return Err(ApiError::not_found("session not found"));
        }
        Ok(SessionDir {
            path: canonical,
            root: self.path.clone(),
        })
    }

    pub(crate) fn list_sessions(&self) -> ApiResult<Vec<String>> {
        list_sessions_at(&self.path).map_err(|e| ApiError::internal(e.to_string()))
    }

    pub(crate) fn create_session(&self, id: &str) -> ApiResult<Session> {
        Session::new_at(self.path.clone(), id.to_string())
            .map_err(|e| ApiError::internal(e.to_string()))
    }

    pub(crate) fn create_unique_session_id(&self) -> ApiResult<String> {
        for _ in 0..64 {
            let id = generate_session_id();
            if !self.path.join(&id).exists() {
                return Ok(id);
            }
        }
        Err(ApiError::internal("failed to generate a unique session id"))
    }
}

/// A verified, containment-checked session directory. All filesystem access
/// for a session goes through this type. Only constructable via
/// `SessionRoot::open_session()` or `SessionDir::subagent_dir()`.
#[derive(Clone)]
pub(crate) struct SessionDir {
    path: PathBuf,
    root: PathBuf,
}

impl SessionDir {
    /// Resolve a relative filename within this session dir. Validates the
    /// component (rejects /, \, .., . prefix, empty, >255 chars), joins to
    /// path, canonicalizes, and verifies the result is still within root.
    ///
    /// This is the single choke-point for all path construction from
    /// user-controlled name components.
    pub(crate) fn safe_path(&self, relative: &str) -> ApiResult<PathBuf> {
        if relative.is_empty() || relative.len() > 255 {
            return Err(ApiError::bad_request("invalid path component"));
        }
        if relative.contains('/')
            || relative.contains('\\')
            || relative.contains("..")
            || relative.starts_with('.')
        {
            return Err(ApiError::bad_request("invalid path component"));
        }
        let resolved = self.path.join(relative);
        self.resolve_contained(resolved)
    }

    /// Find a subagent directory by ID, containment-checked against root.
    pub(crate) fn subagent_dir(&self, id: &str) -> ApiResult<SessionDir> {
        let found = find_subagent_dir_inner(&self.path, id)
            .ok_or_else(|| ApiError::not_found("subagent not found"))?;
        if !found.starts_with(&self.root) {
            return Err(ApiError::bad_request("path traversal detected"));
        }
        Ok(SessionDir {
            path: found,
            root: self.root.clone(),
        })
    }

    /// The canonical session directory. For passing to `AppState` methods
    /// that construct hardcoded sub-paths.
    pub(crate) fn dir(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn read_json<T: DeserializeOwned>(&self, relative: &str) -> ApiResult<T> {
        let path = self.safe_path(relative)?;
        let bytes = tokio::fs::read(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ApiError::not_found(format!("resource {:?} not found", path.file_name()))
            } else {
                ApiError::internal(e.to_string())
            }
        })?;
        serde_json::from_slice(&bytes).map_err(|e| ApiError::internal(e.to_string()))
    }

    pub(crate) async fn read_json_value(&self, relative: &str) -> ApiResult<Value> {
        self.read_json::<Value>(relative).await
    }

    pub(crate) async fn read_optional_text(&self, relative: &str) -> ApiResult<Option<String>> {
        let path = self.safe_path(relative)?;
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ApiError::internal(e.to_string())),
        }
    }

    pub(crate) async fn read_optional_json<T: DeserializeOwned>(
        &self,
        relative: &str,
    ) -> ApiResult<Option<T>> {
        let path = self.safe_path(relative)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| ApiError::internal(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ApiError::internal(e.to_string())),
        }
    }

    pub(crate) fn validate_media_filename(filename: &str) -> ApiResult<()> {
        llm_rs::media::validate_media_filename(filename)
            .map_err(|e| ApiError::bad_request(e.to_string()))
    }

    pub(crate) fn create_media_dir(&self) -> ApiResult<()> {
        let dir = self.path.join("media");
        fs::create_dir_all(&dir)
            .map_err(|e| ApiError::internal(format!("Failed to create media directory: {e}")))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| ApiError::internal(format!("Failed to set permissions: {e}")))?;
        Ok(())
    }

    pub(crate) fn save_media_data(&self, data: &[u8]) -> ApiResult<(String, String)> {
        let session = Session::with_dir(self.path.clone());
        session
            .save_media_data(data)
            .map_err(|e| ApiError::bad_request(e.to_string()))
    }

    pub(crate) fn media_path(&self, filename: &str) -> ApiResult<PathBuf> {
        Self::validate_media_filename(filename)?;
        let resolved = self.path.join("media").join(filename);
        let is_missing = !resolved.exists();
        match self.resolve_contained(resolved) {
            Ok(path) if is_missing => Err(ApiError::not_found("media not found")),
            Ok(path) => Ok(path),
            Err(e) => Err(e),
        }
    }

    /// Canonicalize `path` and verify it is within `self.root`.
    /// When the target does not exist yet, returns the unresolved path —
    /// the validated component cannot escape a canonical, containment-checked
    /// parent.
    fn resolve_contained(&self, path: PathBuf) -> ApiResult<PathBuf> {
        match path.canonicalize() {
            Ok(canonical) => {
                if !canonical.starts_with(&self.root) {
                    return Err(ApiError::bad_request("path traversal detected"));
                }
                Ok(canonical)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path),
            Err(_) => Err(ApiError::bad_request("invalid path")),
        }
    }
}

fn find_subagent_dir_inner(dir: &Path, subagent_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == format!("subagent-{subagent_id}") {
            return Some(path);
        }
        if name.starts_with("subagent-")
            && let Some(found) = find_subagent_dir_inner(&path, subagent_id)
        {
            return Some(found);
        }
    }
    None
}
