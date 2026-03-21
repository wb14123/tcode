use std::path::Path;

use anyhow::Result;

use super::search_grep;

fn test_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/grep_tool")
}

fn temp_dir() -> std::path::PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");
    dir
}

#[test]
fn finds_simple_pattern() -> Result<()> {
    let dir = temp_dir();
    std::fs::write(dir.join("hello.txt"), "hello world\ngoodbye world\n")?;

    let (results, total) = search_grep(&dir, "hello", None)?;
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].lines.len(), 1);
    assert_eq!(results[0].lines[0].line_number, 1);
    assert!(results[0].lines[0].content.contains("hello world"));

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn no_matches_returns_empty() -> Result<()> {
    let dir = temp_dir();
    std::fs::write(dir.join("hello.txt"), "hello world\n")?;

    let (results, total) = search_grep(&dir, "zzz_no_match", None)?;
    assert_eq!(total, 0);
    assert!(results.is_empty());

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn invalid_regex_returns_error() -> Result<()> {
    let dir = temp_dir();
    std::fs::write(dir.join("hello.txt"), "hello\n")?;

    let result = search_grep(&dir, "[invalid", None);
    assert!(result.is_err());

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn include_filter_restricts_files() -> Result<()> {
    let dir = temp_dir();
    std::fs::write(dir.join("match.rs"), "fn main() {}\n")?;
    std::fs::write(dir.join("match.txt"), "fn main() {}\n")?;

    let (results, total) = search_grep(&dir, "fn main", Some("*.rs"))?;
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert!(results[0].path.to_string_lossy().ends_with("match.rs"));

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn long_lines_are_truncated() -> Result<()> {
    let dir = temp_dir();
    let long_line = "x".repeat(3000);
    std::fs::write(dir.join("long.txt"), format!("{}\n", long_line))?;

    let (results, _) = search_grep(&dir, "x+", None)?;
    assert_eq!(results.len(), 1);
    assert!(results[0].lines[0].content.len() <= 2000);

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn results_sorted_by_mtime() -> Result<()> {
    let dir = temp_dir();

    std::fs::write(dir.join("older.txt"), "pattern\n")?;
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(dir.join("newer.txt"), "pattern\n")?;

    let (results, _) = search_grep(&dir, "pattern", None)?;
    assert_eq!(results.len(), 2);
    assert!(results[0].mtime >= results[1].mtime);

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn binary_files_skipped() -> Result<()> {
    let dir = temp_dir();
    std::fs::write(dir.join("binary.dat"), b"hello\x00world\n")?;
    std::fs::write(dir.join("text.txt"), "hello world\n")?;

    let (results, total) = search_grep(&dir, "hello", None)?;
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert!(results[0].path.to_string_lossy().ends_with("text.txt"));

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn hidden_files_searched() -> Result<()> {
    let dir = temp_dir();
    std::fs::write(dir.join(".hidden"), "secret pattern\n")?;

    let (results, total) = search_grep(&dir, "secret pattern", None)?;
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn multiple_matches_in_single_file() -> Result<()> {
    let dir = temp_dir();
    std::fs::write(
        dir.join("multi.txt"),
        "line one match\nno hit here\nline three match\n",
    )?;

    let (results, total) = search_grep(&dir, "match", None)?;
    assert_eq!(total, 2);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].lines.len(), 2);
    assert_eq!(results[0].lines[0].line_number, 1);
    assert_eq!(results[0].lines[1].line_number, 3);

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn regex_pattern_works() -> Result<()> {
    let dir = temp_dir();
    std::fs::write(
        dir.join("code.rs"),
        "fn hello_world() {}\nfn goodbye() {}\n",
    )?;

    let (results, total) = search_grep(&dir, r"fn\s+\w+_\w+", None)?;
    assert_eq!(total, 1);
    assert!(results[0].lines[0].content.contains("hello_world"));

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
