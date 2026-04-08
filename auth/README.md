# auth

OAuth token management for Claude Max. Handles loading, refreshing, and persisting OAuth tokens so that tcode can authenticate against the Anthropic API using a Claude Max subscription.

## Overview

Tokens are stored at `~/.tcode/auth/claude_tokens.json`. The initial OAuth login (PKCE authorization code flow) lives in `tcode/src/claude_auth.rs`; this crate handles everything after that: loading persisted tokens, transparently refreshing them when they expire, and querying subscription usage.

## Key Types

### `OAuthTokens`

Serializable struct holding `access_token`, `refresh_token`, and `expires_at` (unix timestamp). Knows whether it is expired or about to expire (within a 5-minute buffer).

### `TokenManager`

Thread-safe (`Arc<RwLock<OAuthTokens>>`) manager shared across async tasks. Core API:

- `load_from_file(path)` / `load_token_manager()` -- load persisted tokens from disk
- `get_access_token()` -- returns a valid access token, refreshing via the Anthropic token endpoint if necessary (double-checked locking to avoid redundant refreshes)
- `save_tokens()` -- persist current tokens back to disk
- Implements `llm_rs::llm::TokenProvider`, so it plugs directly into the `Claude` LLM backend

## Modules

### `usage`

Fetches Claude subscription rate-limit data from `GET https://api.anthropic.com/api/oauth/usage`.

- `SubscriptionUsage` -- top-level response with optional 5-hour, 7-day, 7-day-sonnet, and 7-day-opus windows
- `UsageWindow` -- utilization percentage (0-100) and optional reset timestamp
- `fetch_usage(client, access_token)` -- makes the API call
- `format_resets_in(resets_at)` -- formats a reset timestamp as a human-readable duration like `"2h 13m"`

## How tcode Uses This

1. `tcode claude-auth` runs the PKCE flow (in `tcode/src/claude_auth.rs`), exchanges the authorization code for tokens, and saves them via `TokenManager::save_tokens()`.
2. On startup, `tcode` calls `auth::load_token_manager()` to load persisted tokens.
3. The `TokenManager` is passed to the `Claude` LLM backend as a `TokenProvider`. Each API request calls `get_access_token()`, which transparently refreshes if needed.
4. The server periodically calls `auth::usage::fetch_usage()` to write rate-limit status to a file for the TUI status bar.
