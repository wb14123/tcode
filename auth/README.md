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

Extends `TokenProvider` with HTTP client access and subscription usage fetching:

```rust
fn client(&self) -> &reqwest::Client;
async fn fetch_rate_limit_snapshot(&self) -> Result<Option<RateLimitSnapshot>>;
async fn fetch_formatted_usage(&self) -> Result<Option<String>>;
```

Implemented by both provider `TokenManager` types so the server can treat them uniformly. `fetch_formatted_usage` is built on `fetch_rate_limit_snapshot`, so each provider has a single fetch-and-parse path and the one-line usage summary can never disagree with the structured data.

### `RateLimitSnapshot` / `RateLimitWindow`

Provider-neutral, serialization-free view of a subscription's rate-limit state:

```rust
pub struct RateLimitWindow {
    pub name: String,                  // "5h", "7d", "primary", "secondary", ...
    pub utilization: f64,              // 0-100
    pub reset_at: Option<DateTime<Utc>>,
}

pub struct RateLimitSnapshot {
    pub windows: Vec<RateLimitWindow>,
    pub limit_reached: bool,
}
```

Each provider's `usage` module converts its own wire payload into a snapshot:

- Claude (`claude::usage`): the four windows of `GET /api/oauth/usage` become `5h`, `7d`, `7d sonnet`, `7d opus`; `resets_at` RFC 3339 timestamps are parsed there. The payload has no explicit "limit reached" flag, so it is derived from any window reporting 100% utilisation.
- OpenAI (`openai::usage`): `primary_window` and `secondary_window` become `primary` and `secondary`; the epoch-seconds `reset_at` is parsed there, and `rate_limit.limit_reached` is carried over.

`RateLimitSnapshot::blocking_reset_at(now)` returns the latest reset among windows at or over 100% utilisation (a request only succeeds once every exhausted window has reset), falling back to the latest reset among all windows when `limit_reached` is set but no window reports 100%. Resets that already passed `now` are ignored, because a snapshot can be minutes old.

### `rate_limit_reset_source` / `SharedTokenProvider`

Two bridges from this crate into `llm-rs`, which cannot know about OAuth or subscription plans:

- `rate_limit_reset_source(Arc<dyn OAuthTokenManager>) -> Arc<dyn llm_rs::llm::RateLimitResetSource>` exposes the usage endpoint as the fallback a provider consults when a failing response carries no reset hint. The answers are cached (30s for a reset time, 5s for "no reset") and a lookup is bounded to 10s, so the provider can share one source with every subagent clone it spawns without hammering a rate-limited endpoint.
- `SharedTokenProvider` wraps the same manager as a `llm_rs::llm::TokenProvider`, so the LLM client and the server state refresh one token set.

Both are constructed by the runtime and handed to the provider; the retry policy itself lives in `llm-rs`.

## Token Storage

Token files are profile-aware and mirror tcode's config profile naming:

| Provider | No Profile | Profile `work` |
|----------|------------|----------------|
| Claude   | `~/.tcode/auth/claude_tokens.json` | `~/.tcode/auth/claude_tokens-work.json` |
| OpenAI   | `~/.tcode/auth/openai_tokens.json` | `~/.tcode/auth/openai_tokens-work.json` |

`TokenManager::storage_path(profile)` is the source of truth for this naming. `TokenManager::load(profile)` loads only the resolved file for that profile and does not fall back to the unsuffixed default file.

## How tcode Uses This

1. **Initial login.** `tcode claude-auth` or `tcode openai-auth` runs the PKCE OAuth flow, exchanges the authorization code for tokens, and saves them via the provider's `TokenManager`. With `tcode -p <profile> ...`, the same auth commands save to the matching profile-specific token file.
2. **Startup.** tcode calls the provider's `TokenManager::load(profile)` to load persisted tokens from disk for the active profile.
3. **Runtime.** The `TokenManager` is passed to the LLM backend as a `TokenProvider` (via `SharedTokenProvider`). Each API request calls `get_access_token()`, which transparently refreshes if needed.
4. **Usage display.** The server periodically calls the provider's usage module to fetch rate-limit status for the TUI status bar.
5. **Rate-limit waiting.** When a `claude-oauth` or `open-ai-oauth` request is rate limited without a usable reset hint in the response, the provider consults `rate_limit_reset_source`, which asks the same token manager for a snapshot of the subscription and returns `blocking_reset_at(now)`.
