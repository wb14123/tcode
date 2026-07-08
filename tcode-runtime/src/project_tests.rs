use std::path::Path;

use crate::project::project_config_dir;

#[test]
fn same_path_produces_same_hash() {
    let a = project_config_dir(Path::new("/home/user/project")).unwrap();
    let b = project_config_dir(Path::new("/home/user/project")).unwrap();
    assert_eq!(a, b);
}

#[test]
fn different_paths_produce_different_hashes() {
    let a = project_config_dir(Path::new("/home/user/project-a")).unwrap();
    let b = project_config_dir(Path::new("/home/user/project-b")).unwrap();
    assert_ne!(a, b);
}

#[test]
fn output_is_under_dot_tcode_projects() {
    let dir = project_config_dir(Path::new("/some/path")).unwrap();
    let path_str = dir.to_string_lossy();
    assert!(path_str.contains(".tcode/projects/"), "got: {path_str}");
}

#[test]
fn hash_is_64_hex_chars() {
    let dir = project_config_dir(Path::new("/some/path")).unwrap();
    let hash_dir = dir.file_name().unwrap().to_string_lossy();
    assert_eq!(hash_dir.len(), 64, "hash length should be 64, got: {hash_dir}");
    assert!(
        hash_dir.chars().all(|c| c.is_ascii_hexdigit()),
        "hash should be all hex digits, got: {hash_dir}"
    );
}

#[test]
fn trailing_slash_is_significant() {
    let a = project_config_dir(Path::new("/home/user/project")).unwrap();
    let b = project_config_dir(Path::new("/home/user/project/")).unwrap();
    assert_ne!(a, b);
}

#[test]
fn non_ascii_paths_do_not_panic() {
    let dir = project_config_dir(Path::new("/home/ユーザー/プロジェクト")).unwrap();
    let hash_dir = dir.file_name().unwrap().to_string_lossy();
    assert_eq!(hash_dir.len(), 64);
    assert!(hash_dir.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash_is_stable_across_calls() {
    let dir1 = project_config_dir(Path::new("/fixed/test/path")).unwrap();
    let dir2 = project_config_dir(Path::new("/fixed/test/path")).unwrap();
    assert_eq!(dir1, dir2);
    // Also verify the hash subdirectory is deterministic
    let hash1 = dir1.file_name().unwrap().to_string_lossy();
    let hash2 = dir2.file_name().unwrap().to_string_lossy();
    assert_eq!(hash1, hash2);
}
