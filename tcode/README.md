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
│  Session dir: ~/.tcode/sessions/{id}/           │
└─────────────────────────────────────────────────┘
```

## CLI Options

### Provider Selection

Use `--provider` to select the LLM provider:

```bash
tcode --provider claude    # Default - uses Claude API
tcode --provider openai    # Uses OpenAI API
tcode --provider openrouter # Uses OpenRouter API
```

Each provider has its own default model, base URL, and environment variable for the API key:

| Provider | Env Variable | Default Model | Default Base URL |
|----------|--------------|---------------|------------------|
| `claude` | `ANTHROPIC_ACCESS_TOKEN` | `claude-opus-4-6` | `https://api.anthropic.com` |
| `openai` | `OPENAI_API_KEY` | `gpt-5-nano` | `https://api.openai.com/v1` |
| `openrouter` | `OPENROUTER_API_KEY` | `deepseek/deepseek-r1` | `https://openrouter.ai/api/v1` |

### Other Options

```bash
--api-key <key>     # Override API key (otherwise uses provider's env var)
--model <model>     # Override default model
--base-url <url>    # Override default base URL
--session <id>      # Session ID (required for subcommands, auto-generated for main command)
```

## Commands

### `tcode`

Starts the server and opens display + edit panes in the current tmux session. Generates a unique 8-character session ID (e.g., `abc12def`) and prints it on startup. Session files persist in `~/.tcode/sessions/{id}/`.

### `tcode [--session <id>] attach`

Attaches to an existing session and resumes the conversation in the current tmux session. If `--session` is omitted, an interactive picker lets you select from available sessions. Must be run inside tmux.

### `tcode sessions`

Lists all sessions with their status (active/inactive). Active sessions have a running server.

### `tcode --session <id> serve`

Starts just the server process (no tmux integration). Requires `--session` flag.

### `tcode --session <id> edit`

Opens a neovim editor for composing and sending messages to the server. Watches `edit-msg.txt` for changes and sends content over the Unix socket.

### `tcode --session <id> display`

Opens a neovim buffer that renders the conversation by tailing `display.jsonl`. Shows user messages, assistant responses, tool calls, and token usage with syntax highlighting.

### `tcode --session <id> tool-call <tool-call-id>`

Opens a neovim buffer showing the detailed output of a specific tool execution. Reads from per-tool-call JSONL files.

### `tcode --session <id> cancel-tool <tool-call-id>`

Cancels a running tool call. Connects to the server's Unix socket, sends a `CancelTool` message, and prints the result. Used by the display pane's `Ctrl-k` keybinding. Works across all conversations (root and subagents) — the `--session` can be a subagent session ID; the root session's socket is resolved automatically.

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

All session data lives in `~/.tcode/sessions/{session_id}/`. Sessions persist after exit.

| File | Purpose |
|------|---------|
| `server.sock` | Unix socket for client-server communication |
| `display.jsonl` | Conversation events (server writes, display reads) |
| `edit-msg.txt` | User message file (edit writes, server reads) |
| `status.txt` | Server status |
| `tool-call-{id}.jsonl` | Per-tool-call output stream |
| `tool-call-{id}-status.txt` | Per-tool-call status |
| `subagent-{conv_id}/` | Sub-session directory for a subagent (same file structure as parent) |
| `debug.log` | Debug logging output |

## Subagent Display

When the LLM spawns a subagent, the server creates a sub-session directory at `{session_dir}/subagent-{conv_id}/` with the same file structure as the parent session (`display.jsonl`, `status.txt`, per-tool-call files). A nested event writer subscribes to the subagent's conversation events and writes them independently.

In the main display, subagent blocks are rendered with `>>> SUB-AGENT: {description}` labels. Pressing `o` on a subagent block opens `tcode --session={parent_id}/subagent-{conv_id} display` in a new tmux window — the same display client used for main conversations. Tool call detail (`o` keybinding) works identically inside subagent displays.

## Display Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `o` | Thinking block | Toggle expand/collapse of thinking content |
| `o` | Subagent block | Open subagent display in a new tmux window |
| `o` | Tool call block | Open tool call detail in a new tmux window |
| `Ctrl-k` | Running tool call | Cancel the tool call (with confirmation popup) |
| `q` | Anywhere | Quit |

Context is determined by cursor position using extmarks. For `Ctrl-k`, a `[Ctrl-k to cancel]` hint is shown on the tool label while the tool is running and removed when it finishes.

## Tool Cancellation

The server maintains a shared map of `tool_call_id -> ConversationClient`, populated by event writers as they process `ToolMessageStart`/`ToolMessageEnd` events. This allows cancellation to work across all conversations (root and subagents) without the display needing to know which conversation owns a tool call.

Flow: `Ctrl-k` in display → confirmation popup → `tcode cancel-tool <id>` CLI → Unix socket → server looks up owning `ConversationClient` → `CancellationToken::cancel()` → tool stream aborted.

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
