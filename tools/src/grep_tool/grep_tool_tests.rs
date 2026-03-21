use std::path::Path;

use super::search_grep;

fn test_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/grep_tool")
}

fn temp_dir() -> std::path::PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn finds_simple_pattern() {
    let dir = temp_dir();
    std::fs::write(dir.join("hello.txt"), "hello world\ngoodbye world\n").unwrap();

    let (results, total) = search_grep(&dir, "hello", None).unwrap();
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].lines.len(), 1);
    assert_eq!(results[0].lines[0].line_number, 1);
    assert!(results[0].lines[0].content.contains("hello world"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn no_matches_returns_empty() {
    let dir = temp_dir();
    std::fs::write(dir.join("hello.txt"), "hello world\n").unwrap();

    let (results, total) = search_grep(&dir, "zzz_no_match", None).unwrap();
    assert_eq!(total, 0);
    assert!(results.is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn invalid_regex_returns_error() {
    let dir = temp_dir();
    std::fs::write(dir.join("hello.txt"), "hello\n").unwrap();

    let result = search_grep(&dir, "[invalid", None);
    assert!(result.is_err());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn include_filter_restricts_files() {
    let dir = temp_dir();
    std::fs::write(dir.join("match.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.join("match.txt"), "fn main() {}\n").unwrap();

    let (results, total) = search_grep(&dir, "fn main", Some("*.rs")).unwrap();
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert!(results[0].path.to_string_lossy().ends_with("match.rs"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn long_lines_are_truncated() {
    let dir = temp_dir();
    let long_line = "x".repeat(3000);
    std::fs::write(dir.join("long.txt"), format!("{}\n", long_line)).unwrap();

    let (results, _) = search_grep(&dir, "x+", None).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].lines[0].content.len() <= 2000);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn results_sorted_by_mtime() {
    let dir = temp_dir();

    std::fs::write(dir.join("older.txt"), "pattern\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(dir.join("newer.txt"), "pattern\n").unwrap();

    let (results, _) = search_grep(&dir, "pattern", None).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].mtime >= results[1].mtime);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn binary_files_skipped() {
    let dir = temp_dir();
    std::fs::write(dir.join("binary.dat"), b"hello\x00world\n").unwrap();
    std::fs::write(dir.join("text.txt"), "hello world\n").unwrap();

    let (results, total) = search_grep(&dir, "hello", None).unwrap();
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert!(results[0].path.to_string_lossy().ends_with("text.txt"));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn hidden_files_searched() {
    let dir = temp_dir();
    std::fs::write(dir.join(".hidden"), "secret pattern\n").unwrap();

    let (results, total) = search_grep(&dir, "secret pattern", None).unwrap();
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn multiple_matches_in_single_file() {
    let dir = temp_dir();
    std::fs::write(
        dir.join("multi.txt"),
        "line one match\nno hit here\nline three match\n",
    )
    .unwrap();

    let (results, total) = search_grep(&dir, "match", None).unwrap();
    assert_eq!(total, 2);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].lines.len(), 2);
    assert_eq!(results[0].lines[0].line_number, 1);
    assert_eq!(results[0].lines[1].line_number, 3);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn regex_pattern_works() {
    let dir = temp_dir();
    std::fs::write(
        dir.join("code.rs"),
        "fn hello_world() {}\nfn goodbye() {}\n",
    )
    .unwrap();

    let (results, total) = search_grep(&dir, r"fn\s+\w+_\w+", None).unwrap();
    assert_eq!(total, 1);
    assert!(results[0].lines[0].content.contains("hello_world"));

    std::fs::remove_dir_all(&dir).unwrap();
}
