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
///
/// Network chunks are accumulated as raw bytes and only complete
/// `\n`-terminated lines are strict-decoded, so a multi-byte UTF-8 character
/// split across chunk boundaries is never corrupted. A line that is not valid
/// UTF-8 terminates the stream with an `Err` (fail loudly rather than show
/// mangled text).
pub(crate) fn sse_stream(
    response: reqwest::Response,
) -> Pin<Box<dyn Stream<Item = Result<SseEvent, String>> + Send>> {
    Box::pin(stream! {
        let mut byte_stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut current_event_type: Option<String> = None;

        while let Some(chunk_result) = byte_stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    yield Err(format!("Stream error: {:?}", e));
                    return;
                }
            };

            for raw_line in tcode_encoding::split_lines(&mut buffer, &chunk) {
                // Trim trailing `\r` / ASCII whitespace from the line BYTES so
                // a CRLF trailing `\r` never reaches the JSON parser.
                let trimmed = raw_line.trim_ascii_end();
                let line = match std::str::from_utf8(trimmed) {
                    Ok(line) => line,
                    Err(err) => {
                        yield Err(format!(
                            "SSE stream contains invalid UTF-8 (first invalid byte at offset {}): {:?}",
                            err.valid_up_to(),
                            trimmed
                        ));
                        return;
                    }
                };

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
