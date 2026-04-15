use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::{
    BaseTokenManager, OAuthProvider, OAuthTokens, TokenRefresher, claude,
    oauth_token_storage_path_in, openai,
};

fn test_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-tmp/auth-token-storage")
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

fn token_fixture(access_token: &str, refresh_token: &str, expires_at: u64) -> OAuthTokens {
    OAuthTokens {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at,
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

fn read_tokens(path: &Path) -> Result<OAuthTokens> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

#[derive(Debug, Clone, Copy)]
struct StaticRefresher;

#[async_trait::async_trait]
impl TokenRefresher for StaticRefresher {
    async fn refresh(&self, _client: &reqwest::Client, refresh_token: &str) -> Result<OAuthTokens> {
        Ok(token_fixture(
            &format!("refreshed-{refresh_token}"),
            &format!("{refresh_token}-next"),
            now_secs() + 3_600,
        ))
    }
}

#[test]
fn claude_default_path_is_unsuffixed() {
    let home_dir = Path::new("/fake-home");

    assert_eq!(
        oauth_token_storage_path_in(home_dir, OAuthProvider::Claude, None),
        PathBuf::from("/fake-home/.tcode/auth/claude_tokens.json")
    );
    assert_eq!(
        claude::TokenManager::storage_path_in(home_dir, None),
        PathBuf::from("/fake-home/.tcode/auth/claude_tokens.json")
    );
}

#[test]
fn claude_profile_path_is_suffixed() {
    let home_dir = Path::new("/fake-home");

    assert_eq!(
        oauth_token_storage_path_in(home_dir, OAuthProvider::Claude, Some("work")),
        PathBuf::from("/fake-home/.tcode/auth/claude_tokens-work.json")
    );
    assert_eq!(
        claude::TokenManager::storage_path_in(home_dir, Some("work")),
        PathBuf::from("/fake-home/.tcode/auth/claude_tokens-work.json")
    );
}

#[test]
fn openai_default_path_is_unsuffixed() {
    let home_dir = Path::new("/fake-home");

    assert_eq!(
        oauth_token_storage_path_in(home_dir, OAuthProvider::OpenAi, None),
        PathBuf::from("/fake-home/.tcode/auth/openai_tokens.json")
    );
    assert_eq!(
        openai::TokenManager::storage_path_in(home_dir, None),
        PathBuf::from("/fake-home/.tcode/auth/openai_tokens.json")
    );
}

#[test]
fn openai_profile_path_is_suffixed() {
    let home_dir = Path::new("/fake-home");

    assert_eq!(
        oauth_token_storage_path_in(home_dir, OAuthProvider::OpenAi, Some("work")),
        PathBuf::from("/fake-home/.tcode/auth/openai_tokens-work.json")
    );
    assert_eq!(
        openai::TokenManager::storage_path_in(home_dir, Some("work")),
        PathBuf::from("/fake-home/.tcode/auth/openai_tokens-work.json")
    );
}

#[test]
fn claude_profile_load_does_not_fall_back_to_default_file() -> Result<()> {
    let home_dir = temp_dir();
    let default_path = claude::TokenManager::storage_path_in(&home_dir, None);

    write_tokens(
        &default_path,
        &token_fixture("default-token", "refresh-token", now_secs() + 3_600),
    )?;

    assert!(claude::TokenManager::load_in(&home_dir, Some("work")).is_none());
    assert!(claude::TokenManager::load_in(&home_dir, None).is_some());
    Ok(())
}

#[test]
fn openai_profile_load_does_not_fall_back_to_default_file() -> Result<()> {
    let home_dir = temp_dir();
    let default_path = openai::TokenManager::storage_path_in(&home_dir, None);

    write_tokens(
        &default_path,
        &token_fixture("default-token", "refresh-token", now_secs() + 3_600),
    )?;

    assert!(openai::TokenManager::load_in(&home_dir, Some("work")).is_none());
    assert!(openai::TokenManager::load_in(&home_dir, None).is_some());
    Ok(())
}

#[tokio::test]
async fn refresh_from_default_path_saves_back_to_default_path() -> Result<()> {
    let home_dir = temp_dir();
    let default_path = claude::TokenManager::storage_path_in(&home_dir, None);
    let profile_path = claude::TokenManager::storage_path_in(&home_dir, Some("work"));

    write_tokens(
        &default_path,
        &token_fixture("old-default", "default-refresh", 0),
    )?;
    write_tokens(
        &profile_path,
        &token_fixture("work-token", "work-refresh", now_secs() + 3_600),
    )?;

    let manager = BaseTokenManager::load_from_file(&default_path, StaticRefresher)
        .expect("default token file should load");

    assert_eq!(
        manager.get_access_token().await?,
        "refreshed-default-refresh"
    );
    assert_eq!(
        read_tokens(&default_path)?.access_token,
        "refreshed-default-refresh"
    );
    assert_eq!(read_tokens(&profile_path)?.access_token, "work-token");
    Ok(())
}

#[tokio::test]
async fn refresh_from_profile_path_saves_back_to_profile_path() -> Result<()> {
    let home_dir = temp_dir();
    let default_path = openai::TokenManager::storage_path_in(&home_dir, None);
    let profile_path = openai::TokenManager::storage_path_in(&home_dir, Some("work"));

    write_tokens(
        &default_path,
        &token_fixture("default-token", "default-refresh", now_secs() + 3_600),
    )?;
    write_tokens(&profile_path, &token_fixture("old-work", "work-refresh", 0))?;

    let manager = BaseTokenManager::load_from_file(&profile_path, StaticRefresher)
        .expect("profile token file should load");

    assert_eq!(manager.get_access_token().await?, "refreshed-work-refresh");
    assert_eq!(
        read_tokens(&profile_path)?.access_token,
        "refreshed-work-refresh"
    );
    assert_eq!(read_tokens(&default_path)?.access_token, "default-token");
    Ok(())
}
