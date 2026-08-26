use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;

use anyhow::Result;
use tcode_runtime::project::{project_config_dir, project_sessions_dir};
use tcode_runtime::session::{Session, SessionMode, record_session_cwd_if_missing};

use super::test_support::{HomeGuard, TestDir, WarnCapture};
use super::{
    confirm_cross_folder_attach, cross_folder_check, cross_folder_unverifiable_text,
    cross_folder_warning_text, read_session_meta_non_creating, repair_session_index,
    validate_attach_target,
};

#[test]
fn mismatch_when_both_paths_canonicalize_and_differ() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let current = dir.path().join("current");
    let other = dir.path().join("other");
    std::fs::create_dir_all(&current)?;
    std::fs::create_dir_all(&other)?;
    let current = std::fs::canonicalize(&current)?;
    let recorded = other.to_str().expect("test paths are UTF-8");
    assert!(!cross_folder_check(Some(recorded), &current)?);
    Ok(())
}

#[test]
fn no_warning_when_paths_are_equal() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let current = std::fs::canonicalize(dir.path())?;
    let recorded = dir.path().to_str().expect("test paths are UTF-8");
    assert!(cross_folder_check(Some(recorded), &current)?);
    Ok(())
}

#[test]
fn unverifiable_when_recorded_cwd_is_none() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let current = std::fs::canonicalize(dir.path())?;
    let err = cross_folder_check(None, &current).expect_err("must be unverifiable");
    assert_eq!(format!("{err}"), "no recorded cwd");
    Ok(())
}

#[test]
fn unverifiable_when_recorded_cwd_folder_was_deleted() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let current = std::fs::canonicalize(dir.path())?;
    let gone = dir.path().join("gone");
    std::fs::create_dir_all(&gone)?;
    std::fs::remove_dir_all(&gone)?;
    let recorded = gone.to_str().expect("test paths are UTF-8");
    let err = cross_folder_check(Some(recorded), &current).expect_err("must be unverifiable");
    assert_eq!(
        format!("{err}"),
        "the recorded working directory no longer exists"
    );
    Ok(())
}

#[test]
fn unverifiable_when_current_dir_cannot_canonicalize() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let recorded_dir = dir.path().join("recorded");
    std::fs::create_dir_all(&recorded_dir)?;
    let recorded = recorded_dir.to_str().expect("test paths are UTF-8");
    // A current dir that does not exist cannot be canonicalized.
    let missing_current = dir.path().join("missing-current");
    let err =
        cross_folder_check(Some(recorded), &missing_current).expect_err("must be unverifiable");
    assert_eq!(format!("{err}"), "cannot resolve the current directory");
    Ok(())
}

#[test]
fn unverifiable_text_includes_id_reason_and_prompt() {
    let text = cross_folder_unverifiable_text("sess-1", "no recorded cwd");
    assert!(text.contains("Cannot verify session sess-1's working directory (no recorded cwd)."));
    assert!(
        text.contains(
            "It may have been started in another folder: the model's context and project"
        )
    );
    assert!(
        text.contains(
            "settings (permissions.json, config.toml) are pinned to wherever it first ran,"
        )
    );
    assert!(text.contains("which may differ from your current folder."));
    assert!(text.contains("Force attach anyway? [y/N]"));
}

#[test]
fn warning_text_includes_id_paths_and_prompt() {
    let text = cross_folder_warning_text("sess-1", "/original/folder", "/current/folder");
    assert!(text.contains("Session sess-1 was started in:"));
    assert!(text.contains("  /original/folder"));
    assert!(text.contains("but you are now in:"));
    assert!(text.contains("  /current/folder"));
    assert!(text.contains("The model's context pins the current directory to /original/folder"));
    assert!(text.contains(
        "Note: this attach will not overwrite the session's recorded working directory;"
    ));
    assert!(text.contains("it stays pinned to the folder shown above."));
    assert!(text.contains("Force attach anyway? [y/N]"));
}

#[test]
fn repair_session_index_is_keyed_by_recorded_cwd() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    let recorded = dir.path().join("recorded");
    std::fs::create_dir_all(&recorded)?;
    // A different folder the attach actually runs from; the entry must still
    // land under the recorded cwd's project.
    let attach_dir = dir.path().join("attach");
    std::fs::create_dir_all(&attach_dir)?;

    repair_session_index(
        "sesskeyd",
        Some(recorded.to_str().expect("test paths are UTF-8")),
    );

    let marker = project_sessions_dir(&recorded)?.join("sesskeyd");
    assert!(marker.is_file(), "marker must exist: {}", marker.display());
    assert_eq!(
        std::fs::metadata(&marker)?.len(),
        0,
        "marker must be zero bytes"
    );
    let foreign = project_sessions_dir(&attach_dir)?.join("sesskeyd");
    assert!(
        !foreign.exists(),
        "entry must never be written under the attach folder"
    );
    Ok(())
}

#[test]
fn repair_session_index_is_a_noop_without_recorded_cwd() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    repair_session_index("sessnwcd", None);

    let projects = home.join(".tcode").join("projects");
    assert!(
        !projects.exists(),
        "no recorded cwd must not write any index entry"
    );
    Ok(())
}

#[test]
fn repair_session_index_is_warn_only_on_failure() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;
    let canonical = std::fs::canonicalize(&cwd)?;
    let project_dir = project_config_dir(&canonical)?;
    std::fs::create_dir_all(&project_dir)?;
    // Block the sessions dir with a regular file so the index write fails.
    std::fs::write(project_dir.join("sessions"), b"not a directory")?;

    let capture = Arc::new(WarnCapture::default());
    tracing::subscriber::with_default(Arc::clone(&capture), || {
        // A repair failure must never block the attach: no error propagates.
        repair_session_index(
            "sessfail",
            Some(cwd.to_str().expect("test paths are UTF-8")),
        );
    });

    let messages = capture.messages();
    assert_eq!(messages.len(), 1, "expected one warning: {messages:?}");
    assert!(
        messages[0].contains("failed to repair session index"),
        "unexpected warning: {}",
        messages[0]
    );
    Ok(())
}

#[test]
fn repair_session_index_skips_subagent_names_silently() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    // A subagent-style id (root + "/subagent-<id>") must be skipped before
    // the index write: no "invalid session id" warning, no marker created.
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;

    let capture = Arc::new(WarnCapture::default());
    tracing::subscriber::with_default(Arc::clone(&capture), || {
        repair_session_index(
            "sesskeyd/subagent-abcdefgh",
            Some(cwd.to_str().expect("test paths are UTF-8")),
        );
    });

    assert_eq!(
        capture.messages(),
        Vec::<String>::new(),
        "a subagent session name must not warn: {:?}",
        capture.messages()
    );
    assert!(
        !home.join(".tcode").join("projects").exists(),
        "a subagent session name must not write any index entry"
    );
    Ok(())
}

#[test]
fn declined_force_attach_still_repairs_index() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    // A session recorded in `work`, with the attach run from a different folder.
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;
    let session = Session::new("sesskept".to_string())?;
    let recorded =
        record_session_cwd_if_missing(session.session_dir(), &cwd)?.expect("test paths are UTF-8");

    // The handler's prep: one non-creating meta read, then the unconditional
    // repair, both before the cross-folder warning check.
    let read_back = read_session_meta_non_creating("sesskept")?.expect("meta was recorded");
    assert_eq!(read_back.cwd.as_deref(), Some(recorded.as_str()));
    repair_session_index("sesskept", read_back.cwd.as_deref());

    // The recorded cwd differs from the attach folder, so the warning would
    // fire; declining it cannot undo the repair that already ran.
    let attach_dir = dir.path().join("attach");
    std::fs::create_dir_all(&attach_dir)?;
    let attach_dir = std::fs::canonicalize(&attach_dir)?;
    assert!(!cross_folder_check(read_back.cwd.as_deref(), &attach_dir)?);
    let marker = project_sessions_dir(&cwd)?.join("sesskept");
    assert!(
        marker.is_file(),
        "a declined force-attach must not undo the index repair"
    );
    Ok(())
}

#[test]
fn web_only_session_never_prompts() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    // Web-only sessions have no recorded cwd by design; the attach must
    // proceed silently instead of prompting as unverifiable. The call returns
    // Ok(true) without reading stdin.
    assert!(confirm_cross_folder_attach(
        "sessweb",
        None,
        Some(SessionMode::WebOnly),
    )?);
    Ok(())
}

#[test]
fn missing_conversation_state_fails_before_any_prompt() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    // A valid-looking id with no session at all: the attach must bail on the
    // conversation-state check without creating a session dir or prompting.
    let err = validate_attach_target("aaaa1111").expect_err("must fail");
    assert_eq!(
        format!("{err}"),
        "No conversation state found for session 'aaaa1111'. Nothing to resume."
    );
    assert!(
        !home.join(".tcode").exists(),
        "validation must not create any session dirs"
    );
    Ok(())
}

#[test]
fn bogus_session_id_fails_without_creating_a_dir() -> Result<()> {
    let dir = TestDir::new("attach_warning");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    let err = validate_attach_target("ABC12345").expect_err("invalid id must fail");
    assert!(format!("{err}").contains("invalid session id"));
    assert!(
        !home.join(".tcode").exists(),
        "an invalid id must not create any session dirs"
    );
    Ok(())
}

/// A non-UTF-8 current directory must still prompt for cross-folder
/// confirmation instead of silently proceeding: the confirmation reads stdin,
/// so this scenario runs in a child process whose stdin answers "n". A
/// decline must surface as `Ok(false)`, never the old silent `Ok(true)`.
const NON_UTF8_CWD_TEST: &str = "non_utf8_current_dir_prompts_instead_of_silently_proceeding";

#[test]
fn non_utf8_current_dir_prompts_instead_of_silently_proceeding() -> Result<()> {
    // Child branch: point the process at a non-UTF-8 current dir and run the
    // attach confirmation against a recorded cwd in a different folder. Exit
    // 0 only when the confirmation fired and the answer "n" declined.
    if let Some(non_utf8_dir) = std::env::var_os("TCODE_NON_UTF8_CWD") {
        let recorded = std::env::var("TCODE_RECORDED_CWD").expect("child needs recorded cwd");
        std::env::set_current_dir(&non_utf8_dir).expect("child sets its own cwd");
        let declined = match confirm_cross_folder_attach("sessutf8", Some(recorded), None) {
            Ok(false) => true,
            Ok(true) => false,
            Err(e) => {
                eprintln!("confirm_cross_folder_attach failed: {e:#}");
                false
            }
        };
        std::process::exit(if declined { 0 } else { 1 });
    }

    let dir = TestDir::new("attach_warning");
    let recorded = dir.path().join("recorded");
    std::fs::create_dir_all(&recorded)?;
    let recorded = recorded.to_str().expect("test paths are UTF-8").to_string();

    // A real directory whose name contains non-UTF-8 bytes.
    let non_utf8_name = std::ffi::OsStr::from_bytes(b"non-utf8-\xff\xfe");
    let non_utf8_dir = dir.path().join(non_utf8_name);
    std::fs::create_dir_all(&non_utf8_dir)?;
    // getcwd in the child resolves symlinks; match it with the canonical form.
    let canonical_dir = std::fs::canonicalize(&non_utf8_dir)?;

    let mut child = std::process::Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg(format!("attach_warning_tests::{NON_UTF8_CWD_TEST}"))
        .arg("--nocapture")
        .env("TCODE_NON_UTF8_CWD", &non_utf8_dir)
        .env("TCODE_RECORDED_CWD", &recorded)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut child_stdin = child.stdin.take().expect("child stdin is piped");
    child_stdin.write_all(b"n\n")?;
    drop(child_stdin);
    let output = child.wait_with_output()?;

    assert!(
        output.status.success(),
        "the attach confirmation must have prompted and declined with 'n'"
    );
    // The warning must render the non-UTF-8 current dir via Debug escaping
    // (lossless), never a lossy replacement.
    let escaped = format!("{canonical_dir:?}");
    assert!(
        output
            .stdout
            .windows(escaped.len())
            .any(|window| window == escaped.as_bytes()),
        "the warning must show the Debug-escaped current dir ({escaped})"
    );
    Ok(())
}
