//! Shared SSE (Server-Sent Events) stream parser for HTTP streaming responses.

use std::pin::Pin;

use async_stream::stream;
use tokio_stream::{Stream, StreamExt};

/// A parsed SSE event.
pub(crate) struct SseEvent {
    /// The event type from `event: <type>` line, if present.
    pub event_type: Option<String>,
    /// The data payload from `data: <payload>` line.
    pub data: String,
}

/// Validate an HTTP response, returning an error string on request failure or non-success status.
pub(crate) async fn check_response(
    result: Result<reqwest::Response, reqwest::Error>,
) -> Result<reqwest::Response, String> {
    let response = result.map_err(|e| format!("Request failed: {:?}", e))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, body));
    }
    Ok(response)
}

/// Parse an HTTP response body as an SSE stream.
///
/// Yields `SseEvent` items for each `data:` line encountered. Handles
/// `event:` lines (associated with the next `data:` line) and empty-line
/// event boundaries per the SSE specification.
pub(crate) fn sse_stream(
    response: reqwest::Response,
) -> Pin<Box<dyn Stream<Item = Result<SseEvent, String>> + Send>> {
    Box::pin(stream! {
        let mut byte_stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut current_event_type: Option<String> = None;

        while let Some(chunk_result) = byte_stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    yield Err(format!("Stream error: {:?}", e));
                    return;
                }
            };

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(line_end) = buffer.find('\n') {
                let line = buffer[..line_end].trim_end().to_string();
                buffer = buffer[line_end + 1..].to_string();

                if line.is_empty() {
                    current_event_type = None;
                    continue;
                }

                if let Some(event_name) = line.strip_prefix("event: ") {
                    current_event_type = Some(event_name.to_string());
                    continue;
                }

                if let Some(data) = line.strip_prefix("data: ") {
                    yield Ok(SseEvent {
                        event_type: current_event_type.take(),
                        data: data.to_string(),
                    });
                }
            }
        }
    })
}
