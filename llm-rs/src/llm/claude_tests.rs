use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::Context;
use base64::Engine;
use serde_json::{Value, json};

use super::claude::convert_messages;
use super::{LLMMessage, ToolCall};
use crate::media::{ContentPart, MediaData};

const IMAGE_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aS1cAAAAASUVORK5CYII=";
const PDF_BYTES: &[u8] = b"%PDF-1.4\n% offline Claude request fixture\n%%EOF\n";

/// Owns one test's fixtures and removes them on success or panic.
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> anyhow::Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/claude");
        std::fs::create_dir_all(&root)?;
        let path = root.join(uuid::Uuid::new_v4().to_string());
        match std::fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        std::fs::create_dir_all(&path)?;
        let dir = Self(path);
        std::fs::write(
            dir.0.join("image.png"),
            base64::engine::general_purpose::STANDARD.decode(IMAGE_BASE64)?,
        )?;
        std::fs::write(dir.0.join("document.pdf"), PDF_BYTES)?;
        Ok(dir)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0)
            && error.kind() != ErrorKind::NotFound
        {
            tracing::warn!(path = ?self.0, %error, "Failed to remove Claude test fixtures");
        }
    }
}

fn text(value: &str) -> ContentPart {
    ContentPart::Text(value.to_string())
}

fn image() -> ContentPart {
    ContentPart::Media(MediaData::new("image.png".into(), "image/png".into()))
}

fn pdf() -> ContentPart {
    ContentPart::Media(MediaData::new(
        "document.pdf".into(),
        "application/pdf".into(),
    ))
}

fn image_block() -> Value {
    json!({
        "type": "image",
        "source": {"type": "base64", "media_type": "image/png", "data": IMAGE_BASE64}
    })
}

fn pdf_block() -> Value {
    json!({
        "type": "document",
        "source": {
            "type": "base64",
            "media_type": "application/pdf",
            "data": base64::engine::general_purpose::STANDARD.encode(PDF_BYTES)
        }
    })
}

fn assistant(ids: &[&str]) -> LLMMessage {
    LLMMessage::Assistant {
        content: String::new(),
        tool_calls: ids
            .iter()
            .map(|id| ToolCall {
                id: (*id).to_string(),
                name: "read".to_string(),
                arguments: r#"{"path":"fixture"}"#.to_string(),
            })
            .collect(),
        raw: None,
    }
}

fn tool_result(id: &str, content: Vec<ContentPart>) -> LLMMessage {
    LLMMessage::ToolResult {
        tool_call_id: id.to_string(),
        content,
    }
}

fn result_block(id: &str, content: Value) -> Value {
    json!({"type": "tool_result", "tool_use_id": id, "content": content})
}

fn serialized_messages(messages: &[LLMMessage], dir: Option<&TestDir>) -> anyhow::Result<Value> {
    let media_dir = dir.map(|dir| dir.0.clone());
    let (_, converted) = convert_messages(messages, &media_dir)?;
    Ok(serde_json::to_value(converted)?)
}

/// Assert exact result IDs, their order, and the absence of top-level media.
fn assert_result_ids_once(messages: &Value, expected: &[&str]) -> anyhow::Result<()> {
    let mut ids = Vec::new();
    for message in messages.as_array().context("messages must be an array")? {
        if message["role"] != "user" {
            continue;
        }
        if let Some(blocks) = message["content"].as_array() {
            for block in blocks {
                assert_eq!(block["type"], "tool_result");
                ids.push(
                    block["tool_use_id"]
                        .as_str()
                        .context("tool result must have an ID")?,
                );
            }
        }
    }
    assert_eq!(ids, expected);
    for id in expected {
        assert_eq!(ids.iter().filter(|actual| *actual == id).count(), 1);
    }
    Ok(())
}

fn assert_two_result_batch(image_first: bool) -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let mut results = vec![
        tool_result("toolu_image", vec![image()]),
        tool_result("toolu_text", vec![text("file contents")]),
    ];
    let mut expected = vec![
        result_block("toolu_image", json!([image_block()])),
        result_block("toolu_text", json!("file contents")),
    ];
    let mut ids = vec!["toolu_image", "toolu_text"];
    if !image_first {
        results.reverse();
        expected.reverse();
        ids.reverse();
    }
    let mut messages = vec![assistant(&ids)];
    messages.extend(results);

    let serialized = serialized_messages(&messages, Some(&dir))?;

    assert_eq!(serialized.as_array().map(Vec::len), Some(2));
    assert_eq!(serialized[0]["role"], "assistant");
    assert_eq!(serialized[1], json!({"role": "user", "content": expected}));
    assert_result_ids_once(&serialized, &ids)
}

#[test]
fn image_result_before_text_result_stays_in_one_user_batch() -> anyhow::Result<()> {
    assert_two_result_batch(true)
}

#[test]
fn image_result_after_text_result_stays_in_one_user_batch() -> anyhow::Result<()> {
    assert_two_result_batch(false)
}

#[test]
fn three_tool_results_batch_with_media_first_middle_or_last() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let ids = ["toolu_first", "toolu_middle", "toolu_last"];
    for media_index in 0..ids.len() {
        let mut messages = vec![assistant(&ids)];
        let mut expected = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let (parts, content) = if index == media_index {
                (vec![image()], json!([image_block()]))
            } else {
                (vec![text("plain result")], json!("plain result"))
            };
            messages.push(tool_result(id, parts));
            expected.push(result_block(id, content));
        }

        let serialized = serialized_messages(&messages, Some(&dir))?;

        assert_eq!(serialized.as_array().map(Vec::len), Some(2));
        assert_eq!(
            serialized[1],
            json!({"role": "user", "content": expected}),
            "media result at index {media_index}"
        );
        assert_result_ids_once(&serialized, &ids)?;
    }
    Ok(())
}

#[test]
fn multiple_media_results_keep_images_and_pdfs_with_their_ids() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let ids = ["toolu_image", "toolu_pdf", "toolu_mixed"];
    let messages = vec![
        assistant(&ids),
        tool_result(ids[0], vec![image()]),
        tool_result(ids[1], vec![pdf()]),
        tool_result(ids[2], vec![pdf(), text("between"), image()]),
    ];

    let serialized = serialized_messages(&messages, Some(&dir))?;

    assert_eq!(serialized.as_array().map(Vec::len), Some(2));
    assert_eq!(
        serialized[1],
        json!({"role": "user", "content": [
            result_block(ids[0], json!([image_block()])),
            result_block(ids[1], json!([pdf_block()])),
            result_block(ids[2], json!([
                pdf_block(), {"type": "text", "text": "between"}, image_block()
            ]))
        ]})
    );
    assert_result_ids_once(&serialized, &ids)
}

#[test]
fn image_only_result_has_no_placeholder_text_or_top_level_image() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let serialized = serialized_messages(
        &[
            assistant(&["toolu_image"]),
            tool_result("toolu_image", vec![image()]),
        ],
        Some(&dir),
    )?;

    assert_eq!(serialized.as_array().map(Vec::len), Some(2));
    assert_eq!(
        serialized[1],
        json!({"role": "user", "content": [
            result_block("toolu_image", json!([image_block()]))
        ]})
    );
    assert_result_ids_once(&serialized, &["toolu_image"])
}

#[test]
fn mixed_result_preserves_text_media_order_and_omits_empty_text_parts() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let cases = [
        (
            vec![text("before"), image()],
            json!([{"type": "text", "text": "before"}, image_block()]),
        ),
        (
            vec![image(), text("after")],
            json!([image_block(), {"type": "text", "text": "after"}]),
        ),
        (vec![text(""), image()], json!([image_block()])),
        (
            vec![image(), text(""), pdf()],
            json!([image_block(), pdf_block()]),
        ),
        (vec![image(), text("")], json!([image_block()])),
        (
            vec![text(""), image(), text(""), pdf(), text("")],
            json!([image_block(), pdf_block()]),
        ),
        (
            vec![
                text(""),
                text("before"),
                image(),
                text(""),
                pdf(),
                text("after"),
                text(""),
            ],
            json!([
                {"type": "text", "text": "before"}, image_block(), pdf_block(),
                {"type": "text", "text": "after"}
            ]),
        ),
    ];
    for (parts, expected) in cases {
        let serialized = serialized_messages(
            &[
                assistant(&["toolu_ordered"]),
                tool_result("toolu_ordered", parts),
            ],
            Some(&dir),
        )?;

        assert_eq!(serialized.as_array().map(Vec::len), Some(2));
        assert_eq!(
            serialized[1],
            json!({"role": "user", "content": [result_block("toolu_ordered", expected)]})
        );
        assert_result_ids_once(&serialized, &["toolu_ordered"])?;
    }
    Ok(())
}

#[test]
fn mixed_result_preserves_whitespace_only_and_padded_text() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let serialized = serialized_messages(
        &[
            assistant(&["toolu_whitespace"]),
            tool_result(
                "toolu_whitespace",
                vec![
                    text(" \t\n"),
                    image(),
                    text(""),
                    text("\n  between \t"),
                    pdf(),
                    text("\r\n\t "),
                ],
            ),
        ],
        Some(&dir),
    )?;

    assert_eq!(serialized.as_array().map(Vec::len), Some(2));
    assert_eq!(
        serialized[1],
        json!({"role": "user", "content": [result_block("toolu_whitespace", json!([
            {"type": "text", "text": " \t\n"}, image_block(),
            {"type": "text", "text": "\n  between \t"}, pdf_block(),
            {"type": "text", "text": "\r\n\t "}
        ]))]})
    );
    assert_result_ids_once(&serialized, &["toolu_whitespace"])
}

#[test]
fn media_results_require_a_media_directory() -> anyhow::Result<()> {
    for media in [image(), pdf()] {
        let error = serialized_messages(&[tool_result("toolu_media", vec![text(""), media])], None)
            .err()
            .context("media result conversion must fail without a media directory")?;

        assert_eq!(
            error.to_string(),
            "Media present in tool result but no media_dir configured"
        );
    }
    Ok(())
}

#[test]
fn media_results_propagate_missing_media_file_errors() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    for (filename, media) in [("image.png", image()), ("document.pdf", pdf())] {
        std::fs::remove_file(dir.0.join(filename))?;
        let error = serialized_messages(
            &[tool_result("toolu_media", vec![text(""), media])],
            Some(&dir),
        )
        .err()
        .context("media result conversion must fail for a missing media file")?;

        let message = error.to_string();
        assert!(message.starts_with("Failed to resolve media path: "));
        assert!(message.ends_with(filename));
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .context("missing media file error must retain its I/O cause")?
                .kind(),
            ErrorKind::NotFound
        );
    }
    Ok(())
}

#[test]
fn media_results_propagate_unreadable_media_file_errors() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    for (filename, media) in [("image.png", image()), ("document.pdf", pdf())] {
        let path = dir.0.join(filename);
        std::fs::remove_file(&path)?;
        // A directory cannot be read as file bytes, even with elevated privileges.
        std::fs::create_dir(&path)?;
        let error = serialized_messages(
            &[tool_result("toolu_media", vec![text(""), media])],
            Some(&dir),
        )
        .err()
        .context("media result conversion must fail for unreadable media")?;

        let message = error.to_string();
        assert!(message.starts_with("Failed to read media file: "));
        assert!(message.ends_with(filename));
        assert!(error.downcast_ref::<std::io::Error>().is_some());
    }
    Ok(())
}

#[test]
fn text_only_results_keep_legacy_strings_without_a_media_directory() -> anyhow::Result<()> {
    let ids = ["toolu_text", "toolu_empty_parts", "toolu_empty_text"];
    let messages = vec![
        assistant(&ids),
        tool_result(ids[0], vec![text("first\n"), text(""), text("second")]),
        tool_result(ids[1], vec![]),
        tool_result(ids[2], vec![text("")]),
    ];

    let serialized = serialized_messages(&messages, None)?;

    assert_eq!(serialized.as_array().map(Vec::len), Some(2));
    assert_eq!(
        serialized[1],
        json!({"role": "user", "content": [
            result_block(ids[0], json!("first\nsecond")),
            result_block(ids[1], json!("")),
            result_block(ids[2], json!(""))
        ]})
    );
    assert_result_ids_once(&serialized, &ids)
}

#[test]
fn ordinary_user_attachments_remain_top_level_and_separate_from_results() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let user_parts = vec![
        text(""),
        text("before"),
        image(),
        text(""),
        text("between"),
        pdf(),
        text("after"),
        text(""),
    ];
    let messages = vec![
        LLMMessage::User(user_parts.clone()),
        tool_result("toolu_image", vec![image()]),
        LLMMessage::User(user_parts),
        tool_result("toolu_pdf", vec![pdf()]),
    ];

    let serialized = serialized_messages(&messages, Some(&dir))?;
    let expected_user = json!({"role": "user", "content": [
        {"type": "text", "text": ""}, {"type": "text", "text": "before"}, image_block(),
        {"type": "text", "text": ""}, {"type": "text", "text": "between"}, pdf_block(),
        {"type": "text", "text": "after"}, {"type": "text", "text": ""}
    ]});

    assert_eq!(
        serialized,
        json!([
            expected_user,
            {"role": "user", "content": [result_block("toolu_image", json!([image_block()]))]},
            expected_user,
            {"role": "user", "content": [result_block("toolu_pdf", json!([pdf_block()]))]}
        ])
    );
    Ok(())
}

#[test]
fn ordinary_user_boundary_splits_consecutive_tool_result_batches() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let ids = ["toolu_first", "toolu_second", "toolu_third", "toolu_fourth"];
    let messages = vec![
        assistant(&ids),
        tool_result(ids[0], vec![image()]),
        tool_result(ids[1], vec![text("second")]),
        LLMMessage::User(vec![text("ordinary user boundary")]),
        tool_result(ids[2], vec![text("third")]),
        tool_result(ids[3], vec![pdf()]),
    ];

    let serialized = serialized_messages(&messages, Some(&dir))?;

    assert_eq!(serialized.as_array().map(Vec::len), Some(4));
    assert_eq!(
        serialized[1],
        json!({"role": "user", "content": [
            result_block(ids[0], json!([image_block()])),
            result_block(ids[1], json!("second"))
        ]})
    );
    assert_eq!(
        serialized[2],
        json!({"role": "user", "content": "ordinary user boundary"})
    );
    assert_eq!(
        serialized[3],
        json!({"role": "user", "content": [
            result_block(ids[2], json!("third")),
            result_block(ids[3], json!([pdf_block()]))
        ]})
    );
    assert_result_ids_once(&serialized, &ids)
}

#[test]
fn assistant_boundary_splits_consecutive_tool_result_batches() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let ids = ["toolu_first", "toolu_second", "toolu_third", "toolu_fourth"];
    for raw_boundary in [false, true] {
        let mut boundary = assistant(&ids[2..]);
        if raw_boundary {
            boundary = LLMMessage::Assistant {
                content: "unused fallback".into(),
                tool_calls: vec![],
                raw: Some(json!({"content": [
                    {"type": "tool_use", "id": ids[2], "name": "read", "input": {}},
                    {"type": "tool_use", "id": ids[3], "name": "mcp_read", "input": {}}
                ]})),
            };
        }
        let messages = vec![
            assistant(&ids[..2]),
            tool_result(ids[0], vec![image()]),
            tool_result(ids[1], vec![text("second")]),
            boundary,
            tool_result(ids[2], vec![text("third")]),
            tool_result(ids[3], vec![pdf()]),
        ];

        let serialized = serialized_messages(&messages, Some(&dir))?;

        assert_eq!(serialized.as_array().map(Vec::len), Some(4));
        assert_eq!(
            serialized[1],
            json!({"role": "user", "content": [
                result_block(ids[0], json!([image_block()])),
                result_block(ids[1], json!("second"))
            ]})
        );
        assert_eq!(serialized[2]["role"], "assistant");
        assert_eq!(serialized[2]["content"][0]["id"], ids[2]);
        assert_eq!(serialized[2]["content"][1]["id"], ids[3]);
        assert_eq!(
            serialized[3],
            json!({"role": "user", "content": [
                result_block(ids[2], json!("third")),
                result_block(ids[3], json!([pdf_block()]))
            ]})
        );
        assert_result_ids_once(&serialized, &ids)?;
    }
    Ok(())
}

#[test]
fn raw_and_reconstructed_assistants_replay_before_nested_media_results() -> anyhow::Result<()> {
    let dir = TestDir::new()?;
    let ids = ["toolu_image", "toolu_pdf"];
    let expected_tools = json!([
        {"type": "tool_use", "id": ids[0], "name": "mcp_read", "input": {"path": "fixture"}},
        {"type": "tool_use", "id": ids[1], "name": "mcp_read", "input": {"path": "fixture"}}
    ]);
    for raw_replay in [false, true] {
        let (assistant, expected_content) = if raw_replay {
            let thinking =
                json!({"type": "thinking", "thinking": "inspect both", "signature": "sig"});
            let expected = json!([
                thinking,
                {"type": "text", "text": "reading files"},
                expected_tools[0], expected_tools[1]
            ]);
            let mut raw_content = expected.clone();
            raw_content[2]["name"] = json!("read");
            (
                LLMMessage::Assistant {
                    content: "must not use fallback".into(),
                    tool_calls: vec![],
                    raw: Some(json!({"content": raw_content})),
                },
                expected,
            )
        } else {
            (assistant(&ids), expected_tools.clone())
        };
        let messages = vec![
            assistant,
            tool_result(ids[0], vec![image()]),
            tool_result(ids[1], vec![pdf()]),
        ];

        let serialized = serialized_messages(&messages, Some(&dir))?;

        assert_eq!(
            serialized,
            json!([
                {"role": "assistant", "content": expected_content},
                {"role": "user", "content": [
                    result_block(ids[0], json!([image_block()])),
                    result_block(ids[1], json!([pdf_block()]))
                ]}
            ])
        );
        assert_result_ids_once(&serialized, &ids)?;
    }
    Ok(())
}
