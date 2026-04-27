use llm_rs::conversation::ConversationSummary;

use crate::session::{
    SessionMeta, SessionMode, ensure_session_mode_initialized, is_valid_session_id,
    read_session_mode, session_meta_from_summary, update_session_meta_from_summary,
    validate_session_id, validate_session_path,
};

fn test_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/session")
}

fn temp_dir() -> anyhow::Result<std::path::PathBuf> {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[test]
fn generated_shape_session_ids_are_valid() {
    assert!(is_valid_session_id("abc123xy"));
    assert!(validate_session_id("abc123xy").is_ok());
    assert!(validate_session_path("abc123xy").is_ok());
}

#[test]
fn path_like_session_ids_are_rejected_as_root_ids() {
    for session_id in [
        "../abcde",
        "abc/1234",
        "abc1234/",
        ".abc1234",
        "ABC123XY",
        "abc123x",
        "abc123xyz",
        "subagent-foo",
    ] {
        assert!(
            !is_valid_session_id(session_id),
            "{session_id} should be invalid"
        );
        assert!(validate_session_id(session_id).is_err());
    }
}

#[test]
fn subagent_session_paths_under_valid_roots_are_valid() {
    assert!(validate_session_path("abc123xy/subagent-conv_123-456").is_ok());
    assert!(validate_session_path("abc123xy/subagent-parent/subagent-child").is_ok());
}

#[test]
fn unsafe_session_paths_are_rejected() {
    for session_id in [
        "abc123xy/../evil",
        "abc123xy/not-subagent",
        "abc123xy/subagent-",
        "abc123xy/subagent-../evil",
        "badroot/subagent-child",
    ] {
        assert!(
            validate_session_path(session_id).is_err(),
            "{session_id} should be invalid"
        );
    }
}

#[test]
fn session_meta_missing_mode_defaults_to_normal() -> anyhow::Result<()> {
    let meta: SessionMeta = serde_json::from_str(
        r#"{
            "description": "old session",
            "created_at": 123,
            "last_active_at": 456
        }"#,
    )?;

    assert_eq!(meta.mode, SessionMode::Normal);
    Ok(())
}

#[test]
fn session_mode_web_only_serializes_as_snake_case() -> anyhow::Result<()> {
    let json = serde_json::to_string(&SessionMode::WebOnly)?;
    assert_eq!(json, r#""web_only""#);
    Ok(())
}

#[test]
fn ensure_session_mode_initialized_preserves_existing_valid_metadata() -> anyhow::Result<()> {
    let dir = temp_dir()?;
    let existing = SessionMeta {
        description: Some("existing".to_string()),
        created_at: Some(30),
        last_active_at: Some(40),
        mode: SessionMode::Normal,
    };
    let meta_path = dir.join("session-meta.json");
    std::fs::write(&meta_path, serde_json::to_string_pretty(&existing)?)?;

    ensure_session_mode_initialized(&dir, SessionMode::WebOnly)?;

    let meta: SessionMeta = serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?;
    assert_eq!(meta.description.as_deref(), Some("existing"));
    assert_eq!(meta.created_at, Some(30));
    assert_eq!(meta.last_active_at, Some(40));
    assert_eq!(meta.mode, SessionMode::Normal);
    Ok(())
}

#[test]
fn ensure_session_mode_initialized_writes_when_metadata_missing() -> anyhow::Result<()> {
    let dir = temp_dir()?;
    ensure_session_mode_initialized(&dir, SessionMode::WebOnly)?;

    let meta: SessionMeta =
        serde_json::from_str(&std::fs::read_to_string(dir.join("session-meta.json"))?)?;
    assert_eq!(meta.mode, SessionMode::WebOnly);
    assert_eq!(read_session_mode(&dir)?, SessionMode::WebOnly);
    assert!(meta.created_at.is_some());
    assert_eq!(meta.created_at, meta.last_active_at);
    Ok(())
}

#[test]
fn session_meta_from_summary_preserves_existing_mode() -> anyhow::Result<()> {
    let dir = temp_dir()?;
    let existing = SessionMeta {
        description: Some("existing".to_string()),
        created_at: Some(1),
        last_active_at: Some(2),
        mode: SessionMode::WebOnly,
    };
    std::fs::write(
        dir.join("session-meta.json"),
        serde_json::to_string_pretty(&existing)?,
    )?;

    let summary = ConversationSummary {
        description: Some("updated".to_string()),
        created_at: Some(3),
        last_active_at: Some(4),
    };
    let saved = session_meta_from_summary(&dir, &summary, SessionMode::Normal)?;

    assert_eq!(saved.description.as_deref(), Some("updated"));
    assert_eq!(saved.created_at, Some(3));
    assert_eq!(saved.last_active_at, Some(4));
    assert_eq!(saved.mode, SessionMode::WebOnly);
    Ok(())
}

#[test]
fn update_session_meta_from_summary_does_not_clobber_shared_tmp_file() -> anyhow::Result<()> {
    let dir = temp_dir()?;
    let shared_tmp = dir.join("session-meta.json.tmp");
    std::fs::write(&shared_tmp, "sentinel")?;
    let summary = ConversationSummary {
        description: Some("updated".to_string()),
        created_at: Some(11),
        last_active_at: Some(22),
    };

    update_session_meta_from_summary(&dir, &summary, SessionMode::Normal)?;

    assert_eq!(std::fs::read_to_string(&shared_tmp)?, "sentinel");
    let from_disk: SessionMeta =
        serde_json::from_str(&std::fs::read_to_string(dir.join("session-meta.json"))?)?;
    assert_eq!(from_disk.description.as_deref(), Some("updated"));
    assert_eq!(from_disk.created_at, Some(11));
    assert_eq!(from_disk.last_active_at, Some(22));
    assert_eq!(from_disk.mode, SessionMode::Normal);
    Ok(())
}

#[test]
fn update_session_meta_from_summary_uses_default_mode_when_missing() -> anyhow::Result<()> {
    let dir = temp_dir()?;
    let summary = ConversationSummary {
        description: Some("new".to_string()),
        created_at: Some(10),
        last_active_at: Some(20),
    };

    let saved = update_session_meta_from_summary(&dir, &summary, SessionMode::WebOnly)?;
    let from_disk: SessionMeta =
        serde_json::from_str(&std::fs::read_to_string(dir.join("session-meta.json"))?)?;

    assert_eq!(saved.mode, SessionMode::WebOnly);
    assert_eq!(from_disk.mode, SessionMode::WebOnly);
    assert_eq!(from_disk.description.as_deref(), Some("new"));
    assert_eq!(from_disk.created_at, Some(10));
    assert_eq!(from_disk.last_active_at, Some(20));
    Ok(())
}
