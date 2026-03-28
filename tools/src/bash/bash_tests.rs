use super::starts_with_cd;

#[test]
fn detects_bare_cd() {
    assert!(starts_with_cd("cd"));
    assert!(starts_with_cd("  cd"));
}

#[test]
fn detects_cd_with_directory() {
    assert!(starts_with_cd("cd /some/dir"));
    assert!(starts_with_cd("cd /some/dir && cargo build"));
    assert!(starts_with_cd("cd /some/dir; cargo build"));
    assert!(starts_with_cd("cd /some/dir\n cargo build"));
}

#[test]
fn detects_cd_with_tab() {
    assert!(starts_with_cd("cd\t/some/dir"));
}

#[test]
fn detects_cd_with_ampersand() {
    assert!(starts_with_cd("cd /tmp&&ls"));
    assert!(starts_with_cd("cd&& ls")); // degenerate but still cd
}

#[test]
fn ignores_non_cd_commands() {
    assert!(!starts_with_cd("cargo build"));
    assert!(!starts_with_cd("ls -la"));
    assert!(!starts_with_cd("cdk deploy"));
    assert!(!starts_with_cd("cdrom"));
    assert!(!starts_with_cd("echo cd /tmp"));
}

#[test]
fn ignores_empty_and_whitespace() {
    assert!(!starts_with_cd(""));
    assert!(!starts_with_cd("   "));
}
