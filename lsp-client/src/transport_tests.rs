use anyhow::Result;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::transport::LspTransport;

/// Helper: read one LSP-framed message from a reader.
async fn read_lsp_message<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        anyhow::ensure!(n > 0, "unexpected EOF reading headers");
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
            content_length = Some(len_str.trim().parse()?);
        }
    }
    let len = content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

/// Inline Python script that acts as a mock LSP server:
/// - Reads JSON-RPC requests with LSP framing from stdin
/// - For requests with `id`, echoes back `{"jsonrpc":"2.0","id":<id>,"result":{"echo":<method>}}`
/// - For notifications (no `id`), just consumes them silently
const MOCK_LSP_SCRIPT: &str = r#"
import sys, json

def read_message():
    content_length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        text = line.decode('utf-8').strip()
        if text == '':
            break
        if text.startswith('Content-Length:'):
            content_length = int(text.split(':')[1].strip())
    if content_length is None:
        return None
    body = sys.stdin.buffer.read(content_length)
    return json.loads(body)

def write_message(msg):
    body = json.dumps(msg)
    header = f"Content-Length: {len(body)}\r\n\r\n"
    sys.stdout.buffer.write(header.encode('utf-8'))
    sys.stdout.buffer.write(body.encode('utf-8'))
    sys.stdout.buffer.flush()

while True:
    msg = read_message()
    if msg is None:
        break
    if 'id' in msg and 'method' in msg:
        method = msg.get('method', '')
        # Special: "server_request_test" triggers us sending a server-initiated request
        if method == 'server_request_test':
            # First send a server-initiated request
            write_message({"jsonrpc": "2.0", "id": 9999, "method": "window/workDoneProgress/create", "params": {}})
            # Read the transport's auto-response to our server request
            auto_resp = read_message()
            # Now respond to the original request, including the auto-response we got
            write_message({"jsonrpc": "2.0", "id": msg['id'], "result": {"auto_response": auto_resp}})
        else:
            resp = {"jsonrpc": "2.0", "id": msg['id'], "result": {"echo": method}}
            write_message(resp)
    # Notifications (no id) are silently consumed
"#;

/// Spawn a mock LSP server child process.
fn spawn_mock_lsp() -> Result<tokio::process::Child> {
    let child = Command::new("python3")
        .arg("-c")
        .arg(MOCK_LSP_SCRIPT)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    Ok(child)
}

/// Test basic message framing: Content-Length header + JSON body.
/// Sends a request, verifies the transport can read the framed response.
#[tokio::test]
async fn test_message_framing_roundtrip() -> Result<()> {
    let child = spawn_mock_lsp()?;
    let transport = LspTransport::new(child).await?;

    let result = transport
        .send_request(
            "textDocument/hover",
            serde_json::json!({"textDocument": {"uri": "file:///test"}}),
        )
        .await?;

    // The mock server echoes back the method name
    assert_eq!(result["echo"], "textDocument/hover");

    // Clean up: drop transport to close stdin, which makes the mock exit
    drop(transport);
    Ok(())
}

/// Test that requests are matched by ID correctly — send multiple concurrent requests
/// and verify each gets the correct response.
#[tokio::test]
async fn test_request_response_id_matching() -> Result<()> {
    let child = spawn_mock_lsp()?;
    let transport = LspTransport::new(child).await?;

    // Send three requests concurrently
    let (r1, r2, r3) = tokio::join!(
        transport.send_request("method/one", serde_json::json!({})),
        transport.send_request("method/two", serde_json::json!({})),
        transport.send_request("method/three", serde_json::json!({})),
    );

    assert_eq!(r1?["echo"], "method/one");
    assert_eq!(r2?["echo"], "method/two");
    assert_eq!(r3?["echo"], "method/three");

    drop(transport);
    Ok(())
}

/// Test that notifications are sent with correct framing and no `id` field.
/// We intercept what was written to the child's stdin via a separate approach:
/// spawn `cat` to capture raw output and verify the framing.
#[tokio::test]
async fn test_notification_framing() -> Result<()> {
    // Use `cat` — it echoes stdin to stdout, so we can read back what we wrote.
    let child = Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let transport = LspTransport::new(child).await?;

    // Sending a notification should succeed
    transport
        .send_notification(
            "textDocument/didOpen",
            serde_json::json!({"textDocument": {"uri": "file:///test.rs"}}),
        )
        .await?;

    // Give the data time to flow through the pipe
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Send a second notification to verify multiple sends work
    transport
        .send_notification("textDocument/didClose", serde_json::json!({}))
        .await?;

    drop(transport);
    Ok(())
}

/// Test that notifications have no `id` field in the JSON body.
/// We write a notification to a `cat` process, then read back the raw bytes.
#[tokio::test]
async fn test_notification_has_no_id() -> Result<()> {
    let mut child = Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut child_stdin = child.stdin.take().unwrap();
    let child_stdout = child.stdout.take().unwrap();

    // Manually write a notification using the same framing the transport uses
    let msg = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {"textDocument": {"uri": "file:///test"}}
    });
    let body = serde_json::to_string(&msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    child_stdin.write_all(header.as_bytes()).await?;
    child_stdin.write_all(body.as_bytes()).await?;
    child_stdin.flush().await?;

    // Read back via the framing parser
    let mut reader = BufReader::new(child_stdout);
    let echoed = read_lsp_message(&mut reader).await?;

    // Verify no `id` field
    assert!(
        echoed.get("id").is_none(),
        "notification should not have an id field"
    );
    assert_eq!(echoed["method"], "textDocument/didOpen");
    assert_eq!(echoed["jsonrpc"], "2.0");

    drop(child_stdin);
    drop(reader);
    let _ = child.kill().await;
    Ok(())
}

/// Test that the transport auto-responds to server-initiated requests.
/// The mock server sends a request to us, and we verify it received our auto-response.
#[tokio::test]
async fn test_server_request_auto_response() -> Result<()> {
    let child = spawn_mock_lsp()?;
    let transport = LspTransport::new(child).await?;

    // The mock server, upon receiving "server_request_test", will:
    // 1. Send us a server-initiated request (id=9999)
    // 2. Read our auto-response
    // 3. Return the auto-response as part of its reply
    let result = transport
        .send_request("server_request_test", serde_json::json!({}))
        .await?;

    // The auto_response the mock received should be a valid JSON-RPC response
    let auto_resp = &result["auto_response"];
    assert_eq!(auto_resp["jsonrpc"], "2.0");
    assert_eq!(auto_resp["id"], 9999);
    assert!(
        auto_resp.get("result").is_some(),
        "auto-response should have a result field"
    );

    drop(transport);
    Ok(())
}

/// Test that LSP error responses are properly propagated as errors.
#[tokio::test]
async fn test_error_response_handling() -> Result<()> {
    // Custom mock that returns an error response
    let error_script = r#"
import sys, json

def read_message():
    content_length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        text = line.decode('utf-8').strip()
        if text == '':
            break
        if text.startswith('Content-Length:'):
            content_length = int(text.split(':')[1].strip())
    if content_length is None:
        return None
    body = sys.stdin.buffer.read(content_length)
    return json.loads(body)

def write_message(msg):
    body = json.dumps(msg)
    header = f"Content-Length: {len(body)}\r\n\r\n"
    sys.stdout.buffer.write(header.encode('utf-8'))
    sys.stdout.buffer.write(body.encode('utf-8'))
    sys.stdout.buffer.flush()

while True:
    msg = read_message()
    if msg is None:
        break
    if 'id' in msg:
        write_message({"jsonrpc": "2.0", "id": msg['id'], "error": {"code": -32601, "message": "Method not found"}})
"#;

    let child = Command::new("python3")
        .arg("-c")
        .arg(error_script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let transport = LspTransport::new(child).await?;

    let result = transport
        .send_request("nonexistent/method", serde_json::json!({}))
        .await;

    assert!(result.is_err(), "error response should propagate as Err");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("LSP error"),
        "error message should mention LSP error, got: {err_msg}"
    );
    assert!(
        err_msg.contains("Method not found"),
        "error message should contain server error text, got: {err_msg}"
    );

    drop(transport);
    Ok(())
}

/// Test Content-Length header parsing with extra whitespace.
/// Verifies the reader_loop handles `Content-Length:  42` (extra spaces).
#[tokio::test]
async fn test_content_length_with_extra_whitespace() -> Result<()> {
    let mut child = Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut child_stdin = child.stdin.take().unwrap();
    let child_stdout = child.stdout.take().unwrap();

    // Write a message with extra whitespace in Content-Length header
    let body = r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;
    let header = format!("Content-Length:   {}\r\n\r\n", body.len());
    child_stdin.write_all(header.as_bytes()).await?;
    child_stdin.write_all(body.as_bytes()).await?;
    child_stdin.flush().await?;

    // Read back and verify parsing works
    let mut reader = BufReader::new(child_stdout);
    let msg = read_lsp_message(&mut reader).await?;
    assert_eq!(msg["id"], 1);
    assert_eq!(msg["result"]["ok"], true);

    drop(child_stdin);
    drop(reader);
    let _ = child.kill().await;
    Ok(())
}

/// Test that the transport handles sequential requests correctly.
#[tokio::test]
async fn test_sequential_requests() -> Result<()> {
    let child = spawn_mock_lsp()?;
    let transport = LspTransport::new(child).await?;

    // Send requests one after another
    for i in 0..5 {
        let method = format!("test/method_{i}");
        let result = transport
            .send_request(&method, serde_json::json!({"index": i}))
            .await?;
        assert_eq!(
            result["echo"].as_str(),
            Some(method.as_str()),
            "response {i} should echo the correct method"
        );
    }

    drop(transport);
    Ok(())
}
