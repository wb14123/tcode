use std::net::{IpAddr, Ipv4Addr};

use crate::config::RemoteConfig;

#[test]
fn with_bind_addr_allows_non_loopback_address() -> anyhow::Result<()> {
    let cfg = RemoteConfig::try_new(8765)?.with_bind_addr(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    assert_eq!(cfg.bind_addr(), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    Ok(())
}

#[test]
fn with_allow_insecure_http_enables_plain_http_cookie_mode() -> anyhow::Result<()> {
    let cfg = RemoteConfig::try_new(8765)?.with_allow_insecure_http(true);
    assert!(cfg.allow_insecure_http);
    Ok(())
}

// ── WebUsersFile parsing tests ───────────────────────────────────────

#[test]
fn web_users_file_deny_unknown_top_level_fields() {
    let toml_str = r#"
unknown_field = "x"

[users.alice]
password_hash = "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$dGVzdGhhc2g="
session_dir = "/tmp/alice-sessions"
trash_dir = "/tmp/alice-trash"
"#;
    let result: Result<crate::config::WebUsersFile, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn web_users_file_deny_unknown_user_fields() {
    let toml_str = r#"
[users.alice]
password_hash = "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$dGVzdGhhc2g="
session_dir = "/tmp/alice-sessions"
trash_dir = "/tmp/alice-trash"
unknown_user_field = "x"
"#;
    let result: Result<crate::config::WebUsersFile, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn web_users_file_valid_toml_parses() -> anyhow::Result<()> {
    let toml_str = r#"
[users.alice]
password_hash = "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$dGVzdGhhc2g="
session_dir = "/tmp/alice-sessions"
trash_dir = "/tmp/alice-trash"

[users.bob]
password_hash = "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$b2ZmZ2hhc2g="
session_dir = "/tmp/bob-sessions"
trash_dir = "/tmp/bob-trash"
"#;
    let file: crate::config::WebUsersFile = toml::from_str(toml_str)?;
    assert_eq!(file.users.len(), 2);
    assert!(file.users.contains_key("alice"));
    assert!(file.users.contains_key("bob"));
    assert_eq!(
        file.users["alice"].password_hash,
        "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$dGVzdGhhc2g="
    );
    assert_eq!(
        file.users["alice"].session_dir,
        std::path::PathBuf::from("/tmp/alice-sessions")
    );
    Ok(())
}

#[test]
fn web_users_file_missing_password_hash_is_rejected() {
    let toml_str = r#"
[users.alice]
session_dir = "/tmp/alice-sessions"
trash_dir = "/tmp/alice-trash"
"#;
    let result: Result<crate::config::WebUsersFile, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn web_users_file_missing_session_dir_is_rejected() {
    let toml_str = r#"
[users.alice]
password_hash = "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$dGVzdGhhc2g="
trash_dir = "/tmp/alice-trash"
"#;
    let result: Result<crate::config::WebUsersFile, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn web_users_file_missing_trash_dir_is_rejected() {
    let toml_str = r#"
[users.alice]
password_hash = "$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$dGVzdGhhc2g="
session_dir = "/tmp/alice-sessions"
"#;
    let result: Result<crate::config::WebUsersFile, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}
