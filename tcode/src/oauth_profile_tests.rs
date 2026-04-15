use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use auth::OAuthTokens;
use parking_lot::Mutex;

use super::{auth_command_for_profile, claude_auth, config, create_llm, openai_auth};

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/tcode-oauth-profile")
}

fn temp_dir() -> PathBuf {
    let dir = test_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&dir).expect("failed to create test dir");
    dir
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_secs()
}

fn token_fixture(access_token: &str) -> OAuthTokens {
    OAuthTokens {
        access_token: access_token.to_string(),
        refresh_token: format!("refresh-{access_token}"),
        expires_at: now_secs() + 3_600,
        account_id: None,
    }
}

fn write_tokens(path: &Path, tokens: &OAuthTokens) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(path, serde_json::to_vec_pretty(tokens)?)?;
    Ok(())
}

fn oauth_config(provider: &str) -> config::TcodeConfig {
    config::TcodeConfig {
        provider: Some(provider.to_string()),
        ..Default::default()
    }
}

fn api_key_config(provider: &str) -> config::TcodeConfig {
    config::TcodeConfig {
        provider: Some(provider.to_string()),
        api_key: Some("test-api-key".to_string()),
        ..Default::default()
    }
}

fn home_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct HomeGuard {
    _guard: parking_lot::MutexGuard<'static, ()>,
    previous_home: Option<OsString>,
}

impl HomeGuard {
    fn set(home_dir: &Path) -> Self {
        let guard = home_env_lock().lock();
        let previous_home = std::env::var_os("HOME");

        // SAFETY: these tests serialize HOME mutation with a process-wide mutex,
        // and only call HOME-dependent code while holding that lock.
        unsafe { std::env::set_var("HOME", home_dir) };

        Self {
            _guard: guard,
            previous_home,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(previous_home) => {
                // SAFETY: see HomeGuard::set; restoration happens while the same
                // process-wide mutex is still held.
                unsafe { std::env::set_var("HOME", previous_home) };
            }
            None => {
                // SAFETY: see HomeGuard::set; restoration happens while the same
                // process-wide mutex is still held.
                unsafe { std::env::remove_var("HOME") };
            }
        }
    }
}

#[test]
fn claude_runtime_uses_profile_specific_tokens_without_fallback() -> Result<()> {
    let home_dir = temp_dir();
    let _home_guard = HomeGuard::set(&home_dir);
    let default_path = claude_auth::token_storage_path(None);

    write_tokens(&default_path, &token_fixture("default-claude-token"))?;

    let (_, _, token_manager) = create_llm(&oauth_config("claude-oauth"), None)?;
    assert!(
        token_manager.is_some(),
        "default profile should still load tokens"
    );

    let err = match create_llm(&oauth_config("claude-oauth"), Some("work")) {
        Ok(_) => {
            anyhow::bail!("profile runtime should not fall back to default Claude OAuth tokens")
        }
        Err(err) => err,
    };
    let err = err.to_string();
    assert!(
        err.contains("claude_tokens-work.json"),
        "expected profile-specific Claude path in error; got {err}",
    );
    assert!(
        err.contains("tcode -p work claude-auth"),
        "expected profile-specific Claude auth command in error; got {err}",
    );

    Ok(())
}

#[test]
fn openai_runtime_uses_profile_specific_tokens_when_profile_is_selected() -> Result<()> {
    let home_dir = temp_dir();
    let _home_guard = HomeGuard::set(&home_dir);
    let profile_path = openai_auth::token_storage_path(Some("work"));

    write_tokens(&profile_path, &token_fixture("work-openai-token"))?;

    let (_, _, token_manager) = create_llm(&oauth_config("open-ai-oauth"), Some("work"))?;
    assert!(
        token_manager.is_some(),
        "selected profile should load its OpenAI OAuth tokens",
    );

    let err = match create_llm(&oauth_config("open-ai-oauth"), None) {
        Ok(_) => anyhow::bail!("default runtime should not load profile-suffixed OpenAI tokens"),
        Err(err) => err,
    };
    let err = err.to_string();
    assert!(
        err.contains("openai_tokens.json"),
        "expected default OpenAI path in error; got {err}",
    );
    assert!(
        err.contains("tcode openai-auth"),
        "expected default OpenAI auth command in error; got {err}",
    );

    Ok(())
}

#[test]
fn auth_commands_resolve_profile_aware_storage_paths() {
    assert_eq!(
        claude_auth::token_storage_path(None)
            .file_name()
            .and_then(|name| name.to_str()),
        Some("claude_tokens.json")
    );
    assert_eq!(
        claude_auth::token_storage_path(Some("work"))
            .file_name()
            .and_then(|name| name.to_str()),
        Some("claude_tokens-work.json")
    );
    assert_eq!(
        openai_auth::token_storage_path(None)
            .file_name()
            .and_then(|name| name.to_str()),
        Some("openai_tokens.json")
    );
    assert_eq!(
        openai_auth::token_storage_path(Some("work"))
            .file_name()
            .and_then(|name| name.to_str()),
        Some("openai_tokens-work.json")
    );
}

#[test]
fn profile_auth_command_strings_are_rendered_correctly() {
    assert_eq!(
        auth_command_for_profile(None, "claude-auth"),
        "tcode claude-auth"
    );
    assert_eq!(
        auth_command_for_profile(Some("work"), "claude-auth"),
        "tcode -p work claude-auth"
    );
    assert_eq!(
        auth_command_for_profile(Some("work"), "openai-auth"),
        "tcode -p work openai-auth"
    );
}

#[test]
fn api_key_providers_remain_unchanged() -> Result<()> {
    for provider in ["claude", "open-ai", "open-router"] {
        let (_, _, token_manager) = create_llm(&api_key_config(provider), Some("work"))?;
        assert!(
            token_manager.is_none(),
            "API-key provider {provider} should not create an OAuth token manager",
        );
    }

    Ok(())
}
