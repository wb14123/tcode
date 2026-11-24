use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;
use std::sync::{Arc, Barrier};

use llm_rs::conversation::ConversationSummary;

use crate::session::{
    MAX_SESSION_NAME_CHARS, SessionMeta, SessionMode, clone_session_name,
    ensure_session_mode_initialized, is_valid_session_id, read_session_meta, read_session_mode,
    record_session_cwd_if_missing, session_meta_from_summary, set_session_name,
    update_session_meta_from_summary, validate_session_id, validate_session_path,
};
use crate::test_support::{TestDir, WarnCapture};

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
    let dir = TestDir::new("session");
    let existing = SessionMeta {
        description: Some("existing".to_string()),
        created_at: Some(30),
        last_active_at: Some(40),
        mode: SessionMode::Normal,
        cwd: None,
        name: None,
    };
    let meta_path = dir.path().join("session-meta.json");
    std::fs::write(&meta_path, serde_json::to_string_pretty(&existing)?)?;

    ensure_session_mode_initialized(dir.path(), SessionMode::WebOnly)?;

    let meta: SessionMeta = serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?;
    assert_eq!(meta.description.as_deref(), Some("existing"));
    assert_eq!(meta.created_at, Some(30));
    assert_eq!(meta.last_active_at, Some(40));
    assert_eq!(meta.mode, SessionMode::Normal);
    Ok(())
}

#[test]
fn ensure_session_mode_initialized_writes_when_metadata_missing() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    ensure_session_mode_initialized(dir.path(), SessionMode::WebOnly)?;

    let meta: SessionMeta = serde_json::from_str(&std::fs::read_to_string(
        dir.path().join("session-meta.json"),
    )?)?;
    assert_eq!(meta.mode, SessionMode::WebOnly);
    assert_eq!(read_session_mode(dir.path())?, SessionMode::WebOnly);
    assert!(meta.created_at.is_some());
    assert_eq!(meta.created_at, meta.last_active_at);
    Ok(())
}

#[test]
fn session_meta_from_summary_preserves_existing_mode() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let existing = SessionMeta {
        description: Some("existing".to_string()),
        created_at: Some(1),
        last_active_at: Some(2),
        mode: SessionMode::WebOnly,
        cwd: None,
        name: None,
    };
    std::fs::write(
        dir.path().join("session-meta.json"),
        serde_json::to_string_pretty(&existing)?,
    )?;

    let summary = ConversationSummary {
        description: Some("updated".to_string()),
        created_at: Some(3),
        last_active_at: Some(4),
    };
    let saved = session_meta_from_summary(dir.path(), &summary, SessionMode::Normal)?;

    assert_eq!(saved.description.as_deref(), Some("updated"));
    assert_eq!(saved.created_at, Some(3));
    assert_eq!(saved.last_active_at, Some(4));
    assert_eq!(saved.mode, SessionMode::WebOnly);
    Ok(())
}

#[test]
fn update_session_meta_from_summary_does_not_clobber_shared_tmp_file() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let shared_tmp = dir.path().join("session-meta.json.tmp");
    std::fs::write(&shared_tmp, "sentinel")?;
    let summary = ConversationSummary {
        description: Some("updated".to_string()),
        created_at: Some(11),
        last_active_at: Some(22),
    };

    update_session_meta_from_summary(dir.path(), &summary, SessionMode::Normal)?;

    assert_eq!(std::fs::read_to_string(&shared_tmp)?, "sentinel");
    let from_disk: SessionMeta = serde_json::from_str(&std::fs::read_to_string(
        dir.path().join("session-meta.json"),
    )?)?;
    assert_eq!(from_disk.description.as_deref(), Some("updated"));
    assert_eq!(from_disk.created_at, Some(11));
    assert_eq!(from_disk.last_active_at, Some(22));
    assert_eq!(from_disk.mode, SessionMode::Normal);
    Ok(())
}

#[test]
fn update_session_meta_from_summary_uses_default_mode_when_missing() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let summary = ConversationSummary {
        description: Some("new".to_string()),
        created_at: Some(10),
        last_active_at: Some(20),
    };

    let saved = update_session_meta_from_summary(dir.path(), &summary, SessionMode::WebOnly)?;
    let from_disk: SessionMeta = serde_json::from_str(&std::fs::read_to_string(
        dir.path().join("session-meta.json"),
    )?)?;

    assert_eq!(saved.mode, SessionMode::WebOnly);
    assert_eq!(from_disk.mode, SessionMode::WebOnly);
    assert_eq!(from_disk.description.as_deref(), Some("new"));
    assert_eq!(from_disk.created_at, Some(10));
    assert_eq!(from_disk.last_active_at, Some(20));
    Ok(())
}

#[test]
fn session_meta_without_cwd_parses_to_none() -> anyhow::Result<()> {
    let meta: SessionMeta = serde_json::from_str(
        r#"{
            "description": "old session",
            "created_at": 123,
            "last_active_at": 456,
            "mode": "normal"
        }"#,
    )?;

    assert_eq!(meta.cwd, None);
    Ok(())
}

#[test]
fn session_meta_cwd_round_trips() -> anyhow::Result<()> {
    let meta = SessionMeta {
        description: Some("desc".to_string()),
        created_at: Some(1),
        last_active_at: Some(2),
        mode: SessionMode::Normal,
        cwd: Some("/work/project".to_string()),
        name: None,
    };

    let json = serde_json::to_string_pretty(&meta)?;
    let parsed: SessionMeta = serde_json::from_str(&json)?;

    assert_eq!(parsed.cwd.as_deref(), Some("/work/project"));
    assert_eq!(parsed.description.as_deref(), Some("desc"));
    assert_eq!(parsed.created_at, Some(1));
    assert_eq!(parsed.last_active_at, Some(2));
    assert_eq!(parsed.mode, SessionMode::Normal);
    Ok(())
}

#[test]
fn record_session_cwd_sets_cwd_when_absent() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");
    std::fs::create_dir_all(&session_dir)?;
    let existing = SessionMeta {
        description: Some("existing".to_string()),
        created_at: Some(30),
        last_active_at: Some(40),
        mode: SessionMode::Normal,
        cwd: None,
        name: None,
    };
    std::fs::write(
        session_dir.join("session-meta.json"),
        serde_json::to_string_pretty(&existing)?,
    )?;

    let recorded = record_session_cwd_if_missing(&session_dir, Path::new("/work/project"))?;
    assert_eq!(recorded.as_deref(), Some("/work/project"));

    let meta: SessionMeta = serde_json::from_str(&std::fs::read_to_string(
        session_dir.join("session-meta.json"),
    )?)?;
    assert_eq!(meta.cwd.as_deref(), Some("/work/project"));
    assert_eq!(meta.description.as_deref(), Some("existing"));
    assert_eq!(meta.created_at, Some(30));
    assert_eq!(meta.last_active_at, Some(40));
    assert_eq!(meta.mode, SessionMode::Normal);
    Ok(())
}

#[test]
fn record_session_cwd_does_not_overwrite_existing_value() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");
    std::fs::create_dir_all(&session_dir)?;
    let existing = SessionMeta {
        description: Some("original".to_string()),
        created_at: Some(1),
        last_active_at: Some(2),
        mode: SessionMode::Normal,
        cwd: Some("/original/project".to_string()),
        name: None,
    };
    std::fs::write(
        session_dir.join("session-meta.json"),
        serde_json::to_string_pretty(&existing)?,
    )?;

    let recorded = record_session_cwd_if_missing(&session_dir, Path::new("/different/project"))?;
    assert_eq!(
        recorded.as_deref(),
        Some("/original/project"),
        "record-once skip must return the pre-existing recorded cwd"
    );

    let meta: SessionMeta = serde_json::from_str(&std::fs::read_to_string(
        session_dir.join("session-meta.json"),
    )?)?;
    assert_eq!(meta.cwd.as_deref(), Some("/original/project"));
    assert_eq!(meta.description.as_deref(), Some("original"));
    Ok(())
}

#[test]
fn record_session_cwd_creates_meta_file_when_missing() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");

    let recorded = record_session_cwd_if_missing(&session_dir, Path::new("/work/project"))?;
    assert_eq!(recorded.as_deref(), Some("/work/project"));

    let meta: SessionMeta = serde_json::from_str(&std::fs::read_to_string(
        session_dir.join("session-meta.json"),
    )?)?;
    assert_eq!(meta.cwd.as_deref(), Some("/work/project"));
    assert_eq!(meta.mode, SessionMode::Normal);
    assert_eq!(meta.description, None);
    assert_eq!(meta.created_at, None);
    assert_eq!(meta.last_active_at, None);
    Ok(())
}

#[test]
fn record_session_cwd_retries_until_partial_meta_is_completed() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");
    std::fs::create_dir_all(&session_dir)?;

    // A concurrent creator writes the meta file in two stages: a partial
    // write (invalid JSON) followed by the complete content once the main
    // thread signals it. The handshake (main polls the raw file until the
    // partial `{` is visible, then signals) replaces a fixed writer sleep,
    // so the record call always starts inside the retry window without
    // depending on wall-clock timing.
    let (complete_tx, complete_rx) = std::sync::mpsc::channel::<()>();
    let writer = {
        let session_dir = session_dir.clone();
        std::thread::spawn(move || -> anyhow::Result<()> {
            std::fs::write(session_dir.join("session-meta.json"), b"{")?;
            complete_rx.recv()?;
            let meta = SessionMeta {
                description: Some("writer meta".to_string()),
                created_at: Some(1),
                last_active_at: Some(2),
                mode: SessionMode::Normal,
                cwd: None,
                name: None,
            };
            std::fs::write(
                session_dir.join("session-meta.json"),
                serde_json::to_string_pretty(&meta)?,
            )?;
            Ok(())
        })
    };

    // Wait until the partial file is on disk, then let the writer finish.
    let meta_path = session_dir.join("session-meta.json");
    loop {
        let content = match std::fs::read(&meta_path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if content.starts_with(b"{") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    complete_tx.send(()).expect("writer thread already exited");

    let recorded = match record_session_cwd_if_missing(&session_dir, Path::new("/work/project")) {
        Ok(recorded) => {
            writer.join().expect("writer thread panicked")?;
            recorded
        }
        Err(e) if e.downcast_ref::<serde_json::Error>().is_some() => {
            // The retry budget was exhausted while the file was still partial
            // (a pathologically slow writer). Join it so the complete meta
            // lands, then record once more.
            writer.join().expect("writer thread panicked")?;
            record_session_cwd_if_missing(&session_dir, Path::new("/work/project"))?
        }
        Err(e) => {
            writer.join().expect("writer thread panicked")?;
            return Err(e);
        }
    };
    assert_eq!(recorded.as_deref(), Some("/work/project"));

    let meta: SessionMeta = serde_json::from_str(&std::fs::read_to_string(
        session_dir.join("session-meta.json"),
    )?)?;
    assert_eq!(meta.cwd.as_deref(), Some("/work/project"));
    assert_eq!(meta.description.as_deref(), Some("writer meta"));
    assert_eq!(meta.created_at, Some(1));
    assert_eq!(meta.last_active_at, Some(2));
    Ok(())
}

#[test]
fn record_session_cwd_concurrent_create_race_serialized_in_process() -> anyhow::Result<()> {
    // META_RMW_LOCK serializes the read-modify-write in-process, so the
    // "race" here is sequential; the create_new + re-read path must still
    // leave one valid meta file recording exactly one contender cwd.
    const NUM_THREADS: usize = 8;

    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");
    let cwds: Vec<String> = (0..NUM_THREADS)
        .map(|i| format!("/work/project-{i}"))
        .collect();

    let barrier = Arc::new(Barrier::new(NUM_THREADS));
    let handles: Vec<_> = cwds
        .iter()
        .map(|cwd| {
            let session_dir = session_dir.clone();
            let cwd = cwd.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                record_session_cwd_if_missing(&session_dir, Path::new(&cwd))
            })
        })
        .collect();

    for handle in handles {
        let recorded = handle.join().expect("thread panicked")?;
        let recorded = recorded.expect("every racer must report a recorded cwd");
        assert!(
            cwds.iter().any(|c| recorded.as_str() == c),
            "recorded cwd {recorded:?} not among the contenders"
        );
    }

    // The final meta file must be valid and record exactly one of the cwds.
    let meta = read_session_meta(&session_dir)?.expect("meta file should exist");
    let recorded = meta.cwd.expect("cwd should be recorded");
    assert!(
        cwds.contains(&recorded),
        "recorded cwd {recorded:?} not among the contenders"
    );
    let again = read_session_meta(&session_dir)?.expect("meta file should exist");
    assert_eq!(again.cwd.as_deref(), Some(recorded.as_str()));
    Ok(())
}

#[test]
fn record_session_cwd_non_utf8_path_is_skipped_with_warning() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let bad_os = OsString::from_vec(vec![0x62, 0x61, 0x64, 0xff]);
    let bad_path = Path::new(&bad_os);

    let capture = Arc::new(WarnCapture::default());
    let result = tracing::subscriber::with_default(Arc::clone(&capture), || {
        record_session_cwd_if_missing(dir.path(), bad_path)
    });
    let recorded = result?;
    assert_eq!(recorded, None, "non-UTF-8 cwd must report nothing recorded");

    assert!(
        !dir.path().join("session-meta.json").exists(),
        "non-UTF-8 cwd must not write a meta file"
    );
    let messages = capture.messages();
    assert_eq!(messages.len(), 1, "expected exactly one warning");
    assert!(
        messages[0].contains("skipping session cwd record"),
        "unexpected warning: {}",
        messages[0]
    );
    Ok(())
}

#[test]
fn record_session_cwd_non_utf8_path_leaves_existing_meta_untouched() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");
    std::fs::create_dir_all(&session_dir)?;
    let existing = SessionMeta {
        description: None,
        created_at: Some(1),
        last_active_at: Some(2),
        mode: SessionMode::Normal,
        cwd: None,
        name: None,
    };
    let original_json = serde_json::to_string_pretty(&existing)?;
    std::fs::write(session_dir.join("session-meta.json"), &original_json)?;

    let bad_os = OsString::from_vec(vec![0x62, 0x61, 0x64, 0xff]);
    let bad_path = Path::new(&bad_os);
    let capture = Arc::new(WarnCapture::default());
    let result = tracing::subscriber::with_default(Arc::clone(&capture), || {
        record_session_cwd_if_missing(&session_dir, bad_path)
    });
    let recorded = result?;
    assert_eq!(recorded, None, "non-UTF-8 cwd must report nothing recorded");

    let on_disk = std::fs::read_to_string(session_dir.join("session-meta.json"))?;
    assert_eq!(
        on_disk, original_json,
        "existing meta file must be unchanged"
    );
    assert_eq!(capture.messages().len(), 1, "expected a warning log");
    Ok(())
}

#[test]
fn record_session_cwd_permanently_corrupt_meta_fails_fast() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");
    std::fs::create_dir_all(&session_dir)?;
    std::fs::write(
        session_dir.join("session-meta.json"),
        b"{ not valid json !!",
    )?;

    let result = record_session_cwd_if_missing(&session_dir, Path::new("/work/project"));

    let err = result.expect_err("a permanently corrupt meta must error out");
    assert!(
        format!("{err:#}").contains("failed to parse"),
        "expected a parse error, got: {err:#}"
    );
    Ok(())
}

#[test]
fn record_session_cwd_io_error_fails_fast() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");
    std::fs::create_dir_all(&session_dir)?;
    // A directory where the meta file should be: reading it is an IO error,
    // which must not be retried.
    std::fs::create_dir_all(session_dir.join("session-meta.json"))?;

    let result = record_session_cwd_if_missing(&session_dir, Path::new("/work/project"));

    let err = result.expect_err("an IO error must fail fast, not retry");
    assert!(
        format!("{err:#}").contains("failed to read"),
        "expected a read error, got: {err:#}"
    );
    Ok(())
}

#[test]
fn summary_update_racing_cwd_record_serialized_cannot_clobber_cwd() -> anyhow::Result<()> {
    // META_RMW_LOCK serializes recorders and updaters in-process; the final
    // meta must still record the cwd, never clobbered by a summary update.
    const RECORDERS: usize = 4;
    const UPDATERS: usize = 4;

    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");
    std::fs::create_dir_all(&session_dir)?;
    let cwd = "/work/project";

    let barrier = Arc::new(Barrier::new(RECORDERS + UPDATERS));
    let mut handles = vec![];

    for _ in 0..RECORDERS {
        let session_dir = session_dir.clone();
        let cwd = cwd.to_string();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || -> anyhow::Result<()> {
            barrier.wait();
            record_session_cwd_if_missing(&session_dir, Path::new(&cwd))?;
            Ok(())
        }));
    }
    for _ in 0..UPDATERS {
        let session_dir = session_dir.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || -> anyhow::Result<()> {
            let summary = ConversationSummary {
                description: Some("racing summary".to_string()),
                created_at: Some(1),
                last_active_at: Some(2),
            };
            barrier.wait();
            update_session_meta_from_summary(&session_dir, &summary, SessionMode::Normal)?;
            Ok(())
        }));
    }

    for handle in handles {
        handle.join().expect("thread panicked")?;
    }

    // The final meta file must be valid and still record the cwd: a summary
    // update must never clobber a just-recorded cwd.
    let meta = read_session_meta(&session_dir)?.expect("meta file should exist");
    assert_eq!(
        meta.cwd.as_deref(),
        Some(cwd),
        "summary update must not clobber the recorded cwd"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// session name
// ---------------------------------------------------------------------------

fn write_meta(session_dir: &Path, meta: &SessionMeta) -> anyhow::Result<()> {
    std::fs::write(
        session_dir.join("session-meta.json"),
        serde_json::to_string_pretty(meta)?,
    )?;
    Ok(())
}

fn meta_with_name(name: Option<&str>) -> SessionMeta {
    SessionMeta {
        description: Some("auto description".to_string()),
        created_at: Some(1),
        last_active_at: Some(2),
        mode: SessionMode::Normal,
        cwd: Some("/work/project".to_string()),
        name: name.map(str::to_string),
    }
}

#[test]
fn session_meta_without_name_parses_to_none() -> anyhow::Result<()> {
    let meta: SessionMeta = serde_json::from_str(
        r#"{
            "description": "old session",
            "created_at": 123,
            "last_active_at": 456,
            "mode": "normal"
        }"#,
    )?;

    assert_eq!(meta.name, None);
    Ok(())
}

#[test]
fn session_meta_name_round_trips() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    write_meta(dir.path(), &meta_with_name(Some("my session")))?;

    let meta = read_session_meta(dir.path())?.expect("meta file should exist");

    assert_eq!(meta.name.as_deref(), Some("my session"));
    assert_eq!(meta.description.as_deref(), Some("auto description"));
    Ok(())
}

#[test]
fn session_meta_from_summary_keeps_existing_name() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    write_meta(dir.path(), &meta_with_name(Some("kept name")))?;

    let summary = ConversationSummary {
        description: Some("updated description".to_string()),
        created_at: Some(3),
        last_active_at: Some(4),
    };
    let saved = session_meta_from_summary(dir.path(), &summary, SessionMode::Normal)?;

    assert_eq!(saved.name.as_deref(), Some("kept name"));
    assert_eq!(saved.description.as_deref(), Some("updated description"));
    Ok(())
}

#[test]
fn update_session_meta_from_summary_keeps_name_on_disk() -> anyhow::Result<()> {
    // The event writer rewrites the meta on each event; a stored name must
    // survive it.
    let dir = TestDir::new("session");
    write_meta(dir.path(), &meta_with_name(Some("kept name")))?;

    let summary = ConversationSummary {
        description: Some("updated description".to_string()),
        created_at: Some(3),
        last_active_at: Some(4),
    };
    update_session_meta_from_summary(dir.path(), &summary, SessionMode::Normal)?;

    let meta = read_session_meta(dir.path())?.expect("meta file should exist");
    assert_eq!(meta.name.as_deref(), Some("kept name"));
    assert_eq!(meta.description.as_deref(), Some("updated description"));
    Ok(())
}

#[test]
fn set_session_name_sets_replaces_and_clears() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");

    set_session_name(&session_dir, Some("first"))?;
    let meta = read_session_meta(&session_dir)?.expect("meta file should exist");
    assert_eq!(meta.name.as_deref(), Some("first"));

    set_session_name(&session_dir, Some("second"))?;
    let meta = read_session_meta(&session_dir)?.expect("meta file should exist");
    assert_eq!(meta.name.as_deref(), Some("second"));

    set_session_name(&session_dir, None)?;
    let meta = read_session_meta(&session_dir)?.expect("meta file should exist");
    assert_eq!(meta.name, None);

    // Clearing an unnamed session changes nothing.
    set_session_name(&session_dir, None)?;
    let meta = read_session_meta(&session_dir)?.expect("meta file should exist");
    assert_eq!(meta.name, None);
    Ok(())
}

#[test]
fn set_session_name_creates_meta_file_when_missing() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let session_dir = dir.path().join("abc123xy");

    set_session_name(&session_dir, Some("written"))?;

    let meta = read_session_meta(&session_dir)?.expect("meta file should exist");
    assert_eq!(meta.name.as_deref(), Some("written"));
    assert_eq!(meta.mode, SessionMode::Normal);
    assert_eq!(meta.description, None);
    Ok(())
}

#[test]
fn set_session_name_keeps_description_and_cwd() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    write_meta(dir.path(), &meta_with_name(None))?;

    set_session_name(dir.path(), Some("new name"))?;

    let meta = read_session_meta(dir.path())?.expect("meta file should exist");
    assert_eq!(meta.name.as_deref(), Some("new name"));
    assert_eq!(meta.description.as_deref(), Some("auto description"));
    assert_eq!(meta.cwd.as_deref(), Some("/work/project"));
    assert_eq!(meta.created_at, Some(1));
    assert_eq!(meta.last_active_at, Some(2));
    Ok(())
}

#[test]
fn set_session_name_trims_surrounding_spaces() -> anyhow::Result<()> {
    let dir = TestDir::new("session");

    set_session_name(dir.path(), Some("  spaced name \n "))?;

    let meta = read_session_meta(dir.path())?.expect("meta file should exist");
    assert_eq!(meta.name.as_deref(), Some("spaced name"));
    Ok(())
}

#[test]
fn set_session_name_accepts_max_length_name() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let name = "a".repeat(MAX_SESSION_NAME_CHARS);

    set_session_name(dir.path(), Some(&name))?;

    let meta = read_session_meta(dir.path())?.expect("meta file should exist");
    assert_eq!(meta.name.as_deref(), Some(name.as_str()));
    Ok(())
}

#[test]
fn set_session_name_rejects_too_long_name() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    let name = "a".repeat(MAX_SESSION_NAME_CHARS + 1);

    let err = set_session_name(dir.path(), Some(&name)).expect_err("65 characters must fail");

    assert_eq!(
        err.to_string(),
        "session name is too long (maximum 64 characters)"
    );
    assert!(
        !dir.path().join("session-meta.json").exists(),
        "a rejected name must not write a meta file"
    );
    Ok(())
}

#[test]
fn set_session_name_rejects_control_characters() -> anyhow::Result<()> {
    let dir = TestDir::new("session");

    for name in ["bad\nname", "bad\tname", "bad\u{1b}name", "bad\u{7f}name"] {
        let err = set_session_name(dir.path(), Some(name))
            .expect_err("control characters must be rejected");
        assert_eq!(
            err.to_string(),
            "session name must not contain control characters",
            "for {name:?}"
        );
    }
    Ok(())
}

#[test]
fn set_session_name_whitespace_only_clears() -> anyhow::Result<()> {
    let dir = TestDir::new("session");
    set_session_name(dir.path(), Some("named"))?;

    set_session_name(dir.path(), Some("   "))?;

    let meta = read_session_meta(dir.path())?.expect("meta file should exist");
    assert_eq!(meta.name, None);
    Ok(())
}

#[test]
fn clone_session_name_appends_suffix() {
    assert_eq!(clone_session_name("my session"), "my session-clone");
}

#[test]
fn clone_session_name_fits_the_max_length() {
    let source = "a".repeat(MAX_SESSION_NAME_CHARS);

    let clone = clone_session_name(&source);

    assert_eq!(clone.chars().count(), MAX_SESSION_NAME_CHARS);
    assert_eq!(
        clone,
        format!("{}-clone", "a".repeat(MAX_SESSION_NAME_CHARS - 6))
    );
}

#[test]
fn clone_session_name_keeps_multibyte_characters_whole() {
    // 64 four-byte characters: the base must drop whole characters, never
    // bytes.
    let source = "漢".repeat(MAX_SESSION_NAME_CHARS);

    let clone = clone_session_name(&source);

    assert_eq!(clone.chars().count(), MAX_SESSION_NAME_CHARS);
    assert_eq!(
        clone,
        format!("{}-clone", "漢".repeat(MAX_SESSION_NAME_CHARS - 6))
    );
}
