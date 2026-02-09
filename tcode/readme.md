# tcode

A terminal-based coding agent that leverages neovim and tmux for its UI. Instead of building a custom TUI, tcode uses tools the user already knows - giving familiar keybindings and extensibility for free.

## How It Works

TCode uses a server-client architecture. The server manages the LLM conversation and writes events to files. Clients are separate neovim processes that read those files and render the UI.

```
┌─────────────────────────────────────────────────┐
│  tmux session                                   │
│                                                 │
│  ┌──────────────────┐  ┌────────────────────┐   │
│  │  Display (nvim)  │  │  Edit (nvim)       │   │
│  │  Reads JSONL     │  │  Writes messages   │   │
│  │  Renders chat    │  │  via Unix socket   │   │
│  └────────┬─────────┘  └────────┬───────────┘   │
│           │ display.jsonl       │ server.sock    │
│           └────────┬────────────┘                │
│                    ▼                             │
│           Server Process                         │
│           ├─ ConversationManager                │
│           ├─ JSONL event writer                  │
│           └─ Unix socket listener               │
│                                                 │
│  Session dir: /tmp/tcode/sessions/{id}/         │
└─────────────────────────────────────────────────┘
```

## Commands

### `tcode`

Starts the server and opens display + edit panes in the current tmux session. One instance per tmux session - running it again attaches to the existing one.

### `tcode serve`

Starts just the server process (no tmux integration). Useful for running components separately.

### `tcode edit`

Opens a neovim editor for composing and sending messages to the server. Watches `edit-msg.txt` for changes and sends content over the Unix socket.

### `tcode display`

Opens a neovim buffer that renders the conversation by tailing `display.jsonl`. Shows user messages, assistant responses, tool calls, and token usage with syntax highlighting.

### `tcode tool-call <id>`

Opens a neovim buffer showing the detailed output of a specific tool execution. Reads from per-tool-call JSONL files.

### `tcode browser`

Launches Chrome with the persistent profile at `~/.tcode/chrome/`. Use this to log in to services (e.g., Kagi for web search) that the headless browser tools need.

### `tcode claude-auth`

Authenticates with Claude via OAuth and outputs tokens for API access. This is for **Claude Pro/Max subscribers** who want to use their subscription credits via the API.

```bash
tcode claude-auth
```

1. Opens authorization URL in your browser (claude.ai)
2. After authorizing, paste the code back into the terminal
3. Outputs OAuth tokens as JSON:

```json
{
  "access_token": "...",
  "refresh_token": "...",
  "expires_at": 1234567890
}
```

## Using Claude OAuth Tokens with the API

Instead of using an API key (`x-api-key`), use the OAuth access token with Bearer authentication:

```bash
curl https://api.anthropic.com/v1/messages \
  -H "Authorization: Bearer <access_token>" \
  -H "anthropic-beta: oauth-2025-04-20" \
  -H "anthropic-version: 2023-06-01" \
  -H "content-type: application/json" \
  -d '{
    "model": "claude-sonnet-4-20250514",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

**Required headers:**
- `Authorization: Bearer <access_token>` - OAuth token (not x-api-key)
- `anthropic-beta: oauth-2025-04-20` - Required for OAuth authentication

**Token refresh:** Access tokens expire (check `expires_at`). Use the `refresh_token` to get a new access token:

```bash
curl -X POST https://console.anthropic.com/v1/oauth/token \
  -H "Content-Type: application/json" \
  -d '{
    "grant_type": "refresh_token",
    "refresh_token": "<refresh_token>",
    "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
  }'
```

## Session Files

All session data lives in `/tmp/tcode/sessions/{session_id}/`:

| File | Purpose |
|------|---------|
| `server.sock` | Unix socket for client-server communication |
| `display.jsonl` | Conversation events (server writes, display reads) |
| `edit-msg.txt` | User message file (edit writes, server reads) |
| `status.txt` | Server status |
| `tool-call-{id}.jsonl` | Per-tool-call output stream |
| `tool-call-{id}-status.txt` | Per-tool-call status |
| `debug.log` | Debug logging output |

## Neovim Plugin

`lua/tcode.lua` is the neovim plugin that powers the display and edit clients. It handles:

- JSONL event parsing and incremental rendering into neovim buffers
- Syntax highlighting (TCodeUser, TCodeAssistant, TCodeTool, TCodeError, TCodeTokens)
- Auto-scrolling (only when cursor is at bottom)
- File watching via inotify
- Token usage display

## Design Notes

The following are design directions, some implemented and some aspirational:

### Message Branching

TCode uses neovim's extmarks to track message boundaries, enabling conversation branching - navigate to a message and branch from there to explore alternative responses.

### Configurable External Tools

The architecture supports configuring different external tools for viewing output, editing, diffs, etc. - anything that can accept tcode's file paths and socket as parameters.

### Diff Viewing

Planned: A `tcode diff` command for reviewing proposed code changes using vimdiff (or a configurable diff tool).
