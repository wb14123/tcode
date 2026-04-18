use axum::{
    extract::Request,
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

#[derive(serde::Serialize)]
struct ErrorBody {
    error: String,
}

pub(crate) async fn require_same_origin(request: Request, next: Next) -> Response {
    let actual_origin = match header_str(request.headers(), header::ORIGIN) {
        Some(origin) => origin,
        None => return next.run(request).await,
    };

    let expected_origin = match expected_origin(request.headers()) {
        Ok(origin) => origin,
        Err(message) => {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(ErrorBody { error: message }),
            )
                .into_response();
        }
    };

    if actual_origin != expected_origin {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(ErrorBody {
                error: format!("origin mismatch: expected {expected_origin}, got {actual_origin}"),
            }),
        )
            .into_response();
    }

    next.run(request).await
}

fn expected_origin(headers: &HeaderMap) -> Result<String, String> {
    let scheme = forwarded_proto(headers)
        .or_else(|| {
            header_str(
                headers,
                axum::http::header::HeaderName::from_static("x-forwarded-proto"),
            )
        })
        .unwrap_or("http");
    let host =
        header_str(headers, header::HOST).ok_or_else(|| "missing Host header".to_string())?;
    Ok(format!("{scheme}://{host}"))
}

fn forwarded_proto(headers: &HeaderMap) -> Option<&str> {
    let forwarded = header_str(headers, header::FORWARDED)?;
    for part in forwarded.split(';').flat_map(|segment| segment.split(',')) {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("proto=") {
            return Some(value.trim_matches('"'));
        }
    }
    None
}

fn header_str(headers: &HeaderMap, name: axum::http::header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}
