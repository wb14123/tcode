use std::path::Path;
use std::sync::{Arc, Barrier};

use crate::project::{ensure_session_indexed, project_config_dir, project_sessions_dir};
use crate::test_support::{HomeGuard, TestDir, WarnCapture, home_env_lock};

#[test]
fn same_path_produces_same_hash() {
    // Both calls must observe the same HOME; a parallel HomeGuard-holding
    // test may redirect it mid-test.
    let _lock = home_env_lock().lock();
    let a = project_config_dir(Path::new("/home/user/project")).unwrap();
    let b = project_config_dir(Path::new("/home/user/project")).unwrap();
    assert_eq!(a, b);
}

#[test]
fn different_paths_produce_different_hashes() {
    // Both calls must observe the same HOME; a parallel HomeGuard-holding
    // test may redirect it mid-test.
    let _lock = home_env_lock().lock();
    let a = project_config_dir(Path::new("/home/user/project-a")).unwrap();
    let b = project_config_dir(Path::new("/home/user/project-b")).unwrap();
    assert_ne!(a, b);
}

#[test]
fn output_is_under_dot_tcode_projects() {
    let _lock = home_env_lock().lock();
    let dir = project_config_dir(Path::new("/some/path")).unwrap();
    let path_str = dir.to_str().unwrap();
    assert!(path_str.contains(".tcode/projects/"), "got: {path_str}");
}

#[test]
fn hash_is_64_hex_chars() {
    let _lock = home_env_lock().lock();
    let dir = project_config_dir(Path::new("/some/path")).unwrap();
    let hash_dir = dir.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        hash_dir.len(),
        64,
        "hash length should be 64, got: {hash_dir}"
    );
    assert!(
        hash_dir.chars().all(|c| c.is_ascii_hexdigit()),
        "hash should be all hex digits, got: {hash_dir}"
    );
}

#[test]
fn trailing_slash_is_significant() {
    // Both calls must observe the same HOME; a parallel HomeGuard-holding
    // test may redirect it mid-test.
    let _lock = home_env_lock().lock();
    let a = project_config_dir(Path::new("/home/user/project")).unwrap();
    let b = project_config_dir(Path::new("/home/user/project/")).unwrap();
    assert_ne!(a, b);
}

#[test]
fn non_ascii_paths_do_not_panic() {
    let _lock = home_env_lock().lock();
    let dir = project_config_dir(Path::new("/home/ユーザー/プロジェクト")).unwrap();
    let hash_dir = dir.file_name().unwrap().to_str().unwrap();
    assert_eq!(hash_dir.len(), 64);
    assert!(hash_dir.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash_is_stable_across_calls() {
    // Both calls must observe the same HOME; a parallel HomeGuard-holding
    // test may redirect it mid-test.
    let _lock = home_env_lock().lock();
    let dir1 = project_config_dir(Path::new("/fixed/test/path")).unwrap();
    let dir2 = project_config_dir(Path::new("/fixed/test/path")).unwrap();
    assert_eq!(dir1, dir2);
    // Also verify the hash subdirectory is deterministic
    let hash1 = dir1.file_name().unwrap().to_str().unwrap();
    let hash2 = dir2.file_name().unwrap().to_str().unwrap();
    assert_eq!(hash1, hash2);
}

#[test]
fn sessions_dir_hashes_canonicalized_cwd() -> anyhow::Result<()> {
    // Both HOME reads (inside project_sessions_dir and project_config_dir)
    // must observe the same value; a parallel HomeGuard-holding test may
    // redirect HOME mid-test.
    let _lock = home_env_lock().lock();
    let dir = TestDir::new("project");
    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;

    let sessions = project_sessions_dir(&cwd)?;
    assert_eq!(
        sessions.file_name().and_then(|n| n.to_str()),
        Some("sessions"),
        "sessions dir must end in a `sessions` component"
    );
    // The hash component must be the same as project_config_dir's hash of
    // the canonicalized cwd.
    let canonical = std::fs::canonicalize(&cwd)?;
    assert_eq!(
        sessions.parent(),
        Some(project_config_dir(&canonical)?.as_path()),
        "sessions dir must live under the project dir of the canonicalized cwd"
    );
    Ok(())
}

#[test]
fn sessions_dir_resolves_symlinked_cwd() -> anyhow::Result<()> {
    // Both calls must observe the same HOME; a parallel HomeGuard-holding
    // test may redirect it mid-test.
    let _lock = home_env_lock().lock();
    let dir = TestDir::new("project");
    let real = dir.path().join("real");
    std::fs::create_dir_all(&real)?;
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link)?;

    assert_eq!(
        project_sessions_dir(&real)?,
        project_sessions_dir(&link)?,
        "symlinked spellings of the same folder must hash identically"
    );
    Ok(())
}

#[test]
fn sessions_dir_un_canonicalizable_cwd_is_an_error() {
    let _lock = home_env_lock().lock();
    let dir = TestDir::new("project");
    let missing = dir.path().join("does-not-exist");
    assert!(
        project_sessions_dir(&missing).is_err(),
        "a cwd that cannot be canonicalized must be an error"
    );
}

#[test]
fn ensure_session_indexed_creates_zero_byte_marker() -> anyhow::Result<()> {
    let dir = TestDir::new("project");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;
    ensure_session_indexed("abc123xy", &cwd)?;

    let marker = project_sessions_dir(&cwd)?.join("abc123xy");
    assert!(marker.is_file(), "marker must exist: {}", marker.display());
    assert_eq!(
        std::fs::metadata(&marker)?.len(),
        0,
        "marker must be zero bytes"
    );
    Ok(())
}

#[test]
fn ensure_session_indexed_is_idempotent() -> anyhow::Result<()> {
    let dir = TestDir::new("project");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;
    ensure_session_indexed("abc123xy", &cwd)?;
    ensure_session_indexed("abc123xy", &cwd)?;

    let marker = project_sessions_dir(&cwd)?.join("abc123xy");
    assert!(marker.is_file());
    assert_eq!(
        std::fs::metadata(&marker)?.len(),
        0,
        "second call must not rewrite the marker"
    );
    Ok(())
}

#[test]
fn ensure_session_indexed_concurrent_create_race_is_safe() -> anyhow::Result<()> {
    const NUM_THREADS: usize = 8;

    let dir = TestDir::new("project");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;

    let barrier = Arc::new(Barrier::new(NUM_THREADS));
    let handles: Vec<_> = (0..NUM_THREADS)
        .map(|_| {
            let cwd = cwd.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ensure_session_indexed("abc123xy", &cwd)
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("thread panicked")?;
    }

    let marker = project_sessions_dir(&cwd)?.join("abc123xy");
    assert!(marker.is_file());
    assert_eq!(
        std::fs::metadata(&marker)?.len(),
        0,
        "concurrent creation must not corrupt the marker"
    );
    Ok(())
}

#[test]
fn ensure_session_indexed_unresolvable_cwd_is_skipped_with_warning() -> anyhow::Result<()> {
    // A cwd that cannot be canonicalized (the folder does not exist) must be
    // skipped with a warning rather than failing the caller.
    let dir = TestDir::new("project");
    let missing = dir.path().join("does-not-exist");

    let capture = Arc::new(WarnCapture::default());
    let result = tracing::subscriber::with_default(Arc::clone(&capture), || {
        ensure_session_indexed("abc123xy", &missing)
    });
    result?;

    let messages = capture.messages();
    assert_eq!(messages.len(), 1, "expected exactly one warning");
    assert!(
        messages[0].contains("failed to canonicalize"),
        "unexpected warning: {}",
        messages[0]
    );
    Ok(())
}

#[test]
fn ensure_session_indexed_rejects_invalid_session_ids() -> anyhow::Result<()> {
    let dir = TestDir::new("project");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;

    let capture = Arc::new(WarnCapture::default());
    let result = tracing::subscriber::with_default(Arc::clone(&capture), || {
        ensure_session_indexed("NOT-VALID!", &cwd)
    });
    result?;

    let messages = capture.messages();
    assert_eq!(messages.len(), 1, "expected exactly one warning");
    assert!(
        messages[0].contains("invalid session id"),
        "unexpected warning: {}",
        messages[0]
    );
    assert!(
        !home.join(".tcode").exists(),
        "an invalid session id must not create any index directories"
    );
    Ok(())
}

#[test]
fn ensure_session_indexed_creates_sessions_dir_with_0700_permissions() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new("project");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let _guard = HomeGuard::set(&home);

    let cwd = dir.path().join("work");
    std::fs::create_dir_all(&cwd)?;
    ensure_session_indexed("abc123xy", &cwd)?;

    let sessions_dir = project_sessions_dir(&cwd)?;
    let mode = std::fs::metadata(&sessions_dir)?.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "sessions index dir must be 0700, got {:o}",
        mode
    );
    Ok(())
}
