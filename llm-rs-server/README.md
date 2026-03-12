# llm-rs-server

OpenAI-compatible API proxy that exposes any llm-rs provider behind the standard Chat Completions API. Point any OpenAI client SDK at this server to use Claude, OpenAI, or OpenRouter as the backend.

## Usage

```bash
# OpenRouter (default) — on first run, auto-generates a token and prints it
cargo run -p llm-rs-server -- --provider open-router

# Claude with API key
ANTHROPIC_API_KEY=sk-... cargo run -p llm-rs-server -- --provider claude

# Claude with OAuth (reuses tokens from `tcode claude-auth`)
cargo run -p llm-rs-server -- --provider claude

# OpenAI with custom bind address
cargo run -p llm-rs-server -- --provider open-ai --api-key sk-... --bind 0.0.0.0:3000
```

Then use any OpenAI-compatible client:

```bash
curl http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <token-from-tokens.json>" \
  -d '{
    "model": "deepseek/deepseek-r1",
    "messages": [{"role": "user", "content": "Hello"}],
    "stream": true
  }'
```

## Authentication

All requests require a valid `Authorization: Bearer <token>` header. Tokens are managed via a JSON file:

- **Default path**: `~/.config/llm-rs-server/tokens.json`
- **Override**: `--token-file /path/to/tokens.json`
- **Format**: JSON array of allowed token strings

```json
["token-for-app-a", "token-for-app-b"]
```

The server refuses to start if the file is missing or empty, and prints a command to create it.

## CLI Options

| Option | Default | Description |
|--------|---------|-------------|
| `--provider` | `open-router` | Upstream provider: `claude`, `open-ai`, `open-router` |
| `--api-key` | Provider env var | API key (or set provider env var; Claude also falls back to OAuth tokens) |
| `--base-url` | Provider default | Override upstream API URL |
| `--bind` | `127.0.0.1:8080` | Server listen address |
| `--token-file` | `~/.config/llm-rs-server/tokens.json` | Path to allowed bearer tokens file |

## Endpoints

### `POST /v1/chat/completions`

Standard OpenAI Chat Completions API. Supports:

- Streaming (`"stream": true`) and non-streaming responses
- Tool/function calling (passthrough — server does NOT execute tools)
- `stream_options.include_usage` for token counts in streaming
- `reasoning` field (effort, max_tokens, exclude) for thinking models

### `GET /v1/models`

Returns available models from the configured provider.

## How It Works

```
Client (OpenAI SDK)  →  llm-rs-server  →  Upstream Provider
                                            (Claude / OpenAI / OpenRouter)

1. Receive OpenAI-format request
2. Convert to llm-rs types (LLMMessage, ChatOptions, sentinel Tools)
3. Forward to provider via LLM::chat()
4. Convert LLMEvent stream back to OpenAI SSE chunks (or accumulate for non-streaming)
```

Each request clones the provider instance and registers any tool definitions as sentinel tools (schema-only, no execution). The model string is passed through directly to the upstream provider.

## Modules

- **`types`** — OpenAI wire format serde types (request, response, streaming chunks)
- **`convert`** — Bidirectional conversion between OpenAI types and llm-rs `LLMMessage` / `ChatOptions` / `Tool`
- **`stream`** — LLMEvent stream → SSE chunks with `[DONE]` sentinel, and non-streaming accumulator
- **`handler`** — Axum route handlers and router construction
- **`auth`** — Mandatory bearer token middleware with token file management
- **`error`** — `AppError` → OpenAI-format error JSON responses
- **`claude_auth`** — Claude OAuth token loading with auto-refresh (reuses `~/.tcode/auth/claude_tokens.json`)
- **`config`** — Provider enum with default URLs and env var names
