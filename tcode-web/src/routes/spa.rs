use std::path::{Path, PathBuf};

use axum::{
    body::Body,
    http::{Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};

pub(crate) async fn serve_frontend(method: Method, uri: Uri) -> Response {
    let request_path = uri.path();
    if is_api_path(request_path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let dist_dir = frontend_dist_dir();
    let dist_available = match path_is_dir(&dist_dir).await {
        Ok(is_dir) => is_dir,
        Err(error) => {
            tracing::warn!(path = %dist_dir.display(), %error, "failed to inspect frontend dist directory");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    if !dist_available {
        return StatusCode::NOT_FOUND.into_response();
    }

    if method != Method::GET && method != Method::HEAD {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }

    let relative_path = match normalize_relative_path(request_path) {
        Some(path) => path,
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let response_path = match resolve_response_path(&dist_dir, &relative_path).await {
        Ok(Some(path)) => path,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::warn!(path = %dist_dir.display(), %error, "failed to resolve frontend response path");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    match file_response(&response_path, method == Method::HEAD).await {
        Ok(response) => response,
        Err((status, error)) => {
            tracing::warn!(path = %response_path.display(), %error, "failed to serve frontend file");
            status.into_response()
        }
    }
}

fn frontend_dist_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("frontend/dist")
}

fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

fn normalize_relative_path(request_path: &str) -> Option<PathBuf> {
    let trimmed = request_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Some(PathBuf::from("index.html"));
    }

    let mut relative = PathBuf::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." || segment.contains('\\') {
            return None;
        }
        relative.push(segment);
    }

    if relative.as_os_str().is_empty() {
        return Some(PathBuf::from("index.html"));
    }

    Some(relative)
}

async fn resolve_response_path(
    dist_dir: &Path,
    relative_path: &Path,
) -> std::io::Result<Option<PathBuf>> {
    let candidate = dist_dir.join(relative_path);
    if path_is_file(&candidate).await? {
        return Ok(Some(candidate));
    }

    if should_fallback_to_index(relative_path) {
        let index_path = dist_dir.join("index.html");
        if path_is_file(&index_path).await? {
            return Ok(Some(index_path));
        }
    }

    Ok(None)
}

fn should_fallback_to_index(relative_path: &Path) -> bool {
    if relative_path
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        == Some("assets")
    {
        return false;
    }

    relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_none_or(|name| !name.contains('.'))
}

async fn path_is_dir(path: &Path) -> std::io::Result<bool> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

async fn path_is_file(path: &Path) -> std::io::Result<bool> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

async fn file_response(
    path: &Path,
    head_only: bool,
) -> Result<Response, (StatusCode, std::io::Error)> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|error| map_io_error(path, error))?;
    let content_type = content_type_for_path(path);
    let content_length = bytes.len().to_string();

    let body = if head_only {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    if let Ok(content_length) = header::HeaderValue::from_str(&content_length) {
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, content_length);
    }
    if path.extension().and_then(|ext| ext.to_str()) == Some("html") {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-cache"),
        );
    }
    Ok(response)
}

fn map_io_error(path: &Path, error: std::io::Error) -> (StatusCode, std::io::Error) {
    let status = if error.kind() == std::io::ErrorKind::NotFound {
        StatusCode::NOT_FOUND
    } else {
        tracing::warn!(path = %path.display(), %error, "frontend file read failed");
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (status, error)
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("gif") => "image/gif",
        Some("html") => "text/html; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("jpeg" | "jpg") => "image/jpeg",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("map") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}
