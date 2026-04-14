# auth

OAuth token management for LLM providers (Claude and OpenAI). Handles loading, refreshing, and persisting OAuth tokens so that tcode can authenticate against provider APIs using subscription credentials.

## Architecture

The crate is split into a shared generic core and provider-specific submodules:

```
auth/src/
  lib.rs          # OAuthTokens, TokenRefresher trait, BaseTokenManager<R>, OAuthTokenManager trait
  claude/
    mod.rs        # ClaudeRefresher, TokenManager type alias, load_token_manager()
    usage.rs      # Claude subscription usage (rate-limit windows)
  openai/
    mod.rs        # OpenAiRefresher, TokenManager type alias, load_token_manager()
    usage.rs      # OpenAI subscription usage
```

## Key Types

### `OAuthTokens`

Serializable struct holding `access_token`, `refresh_token`, `expires_at` (unix timestamp), and an optional `account_id` (used by OpenAI). Knows whether it is expired or about to expire (within a 5-minute buffer).

### `TokenRefresher` trait

Provider-specific token refresh logic. Each provider implements a single async method:

```rust
async fn refresh(&self, client: &reqwest::Client, refresh_token: &str) -> Result<OAuthTokens>;
```

Implementations: `ClaudeRefresher` (calls the Anthropic token endpoint) and `OpenAiRefresher` (calls the OpenAI token endpoint).

### `BaseTokenManager<R: TokenRefresher>`

Generic, thread-safe (`Arc<RwLock<OAuthTokens>>`) token manager parameterized over a `TokenRefresher`. Handles:

- Loading tokens from a JSON file on disk
- Persisting tokens with 0600 permissions
- Double-checked-locking refresh (avoids redundant concurrent refreshes)
- Implements `llm_rs::llm::TokenProvider`, so it plugs directly into LLM backends

Each provider module exposes a `TokenManager` type alias (e.g., `claude::TokenManager = BaseTokenManager<ClaudeRefresher>`).

### `OAuthTokenManager` trait

Extends `TokenProvider` with HTTP client access and formatted usage fetching. Implemented by both provider `TokenManager` types so the server can treat them uniformly.

## Token Storage

| Provider | Token File |
|----------|------------|
| Claude   | `~/.tcode/auth/claude_tokens.json` |
| OpenAI   | `~/.tcode/auth/openai_tokens.json` |

## How tcode Uses This

1. **Initial login.** `tcode claude-auth` or `tcode openai-auth` runs the PKCE OAuth flow, exchanges the authorization code for tokens, and saves them via the provider's `TokenManager`.
2. **Startup.** tcode calls the provider's `load_token_manager()` to load persisted tokens from disk.
3. **Runtime.** The `TokenManager` is passed to the LLM backend as a `TokenProvider`. Each API request calls `get_access_token()`, which transparently refreshes if needed.
4. **Usage display.** The server periodically calls the provider's usage module to fetch rate-limit status for the TUI status bar.
