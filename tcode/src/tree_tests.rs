use std::fs::File;
use std::io::Write;

use crate::tree::TreeState;
use tcode_encoding::split_lines;

/// Per-test temp dir under the workspace target dir; removed on drop
/// (cleanup runs on success and on panic).
struct TestDir(std::path::PathBuf);

impl TestDir {
    fn new(module: &str) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../target/test-tmp/{module}"));
        std::fs::create_dir_all(&root).expect("failed to create test root");
        let dir = root.join(uuid::Uuid::new_v4().to_string());
        // Cleanup before: remove any stale leftover at this exact path.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn split_char_across_appends_roundtrips() {
    // `世` is E4 B8 96: the first append ends mid-character, the second
    // append completes it. The extracted line must round-trip exactly with
    // zero U+FFFD.
    let mut buffer = Vec::new();
    let first = split_lines(&mut buffer, b"data: \xe4\xb8");
    assert!(first.is_empty(), "no complete line after the first append");
    assert_eq!(buffer, b"data: \xe4\xb8");

    let lines = split_lines(&mut buffer, b"\x96\nnext");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], b"data: \xe4\xb8\x96");
    let decoded = std::str::from_utf8(&lines[0]).expect("complete line must be valid UTF-8");
    assert_eq!(decoded, "data: 世");
    assert!(!decoded.contains('\u{FFFD}'), "no replacement chars");

    // The trailing unterminated partial line stays in the buffer as raw bytes.
    assert_eq!(buffer, b"next");
}

#[test]
fn invalid_line_is_detectable() {
    let mut buffer = Vec::new();
    let lines = split_lines(&mut buffer, b"valid line\nbad \xff line\n");
    assert_eq!(lines.len(), 2);
    assert!(std::str::from_utf8(&lines[0]).is_ok());
    let err = std::str::from_utf8(&lines[1]).unwrap_err();
    assert_eq!(err.error_len(), Some(1));
    assert!(buffer.is_empty());
}

#[test]
fn dual_parse_new_and_legacy_lines() -> anyhow::Result<()> {
    let dir = TestDir::new("tree");
    let display_file = dir.path().join("display.jsonl");
    let new_format = r#"{"id": 5, "msg": {"AssistantToolCallStart": {"tool_call_index": 0, "tool_call_id": "t1", "tool_name": "bash", "created_at": 0}}}"#;
    let legacy_format = r#"{"AssistantToolCallStart": {"msg_id": 3, "tool_call_index": 1, "tool_call_id": "t2", "tool_name": "bash", "created_at": 0}}"#;
    let mut file = File::create(&display_file)?;
    writeln!(file, "{new_format}")?;
    writeln!(file, "{legacy_format}")?;
    drop(file);

    let mut state = TreeState::new(dir.path().to_path_buf(), "test-session".to_string());
    state.read_file(&display_file);

    assert!(state.tool_call_idx.contains_key("t1"));
    assert!(state.tool_call_idx.contains_key("t2"));
    Ok(())
}
