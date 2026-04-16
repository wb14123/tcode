use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::auth::SessionStatus;
use super::build_router;
use crate::state::AppState;

#[tokio::test]
async fn get_session_returns_unauthenticated() -> anyhow::Result<()> {
    let state = Arc::new(AppState::new("valid-password-16chars!".into()));
    let app = build_router(state);

    let request = Request::builder()
        .uri("/api/auth/session")
        .body(Body::empty())?;

    let response = app.oneshot(request).await?;

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.starts_with("application/json"),
        "unexpected content-type: {content_type}"
    );

    let body_bytes = response.into_body().collect().await?.to_bytes();
    let parsed: SessionStatus = serde_json::from_slice(&body_bytes)?;
    assert_eq!(
        parsed,
        SessionStatus {
            authenticated: false
        }
    );

    Ok(())
}
