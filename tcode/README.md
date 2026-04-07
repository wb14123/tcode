# tcode

A terminal-based coding agent that leverages neovim and tmux for its UI. Instead of building a custom TUI, tcode uses tools the user already knows - giving familiar keybindings and extensibility for free.

## How It Works

TCode uses a server-client architecture. The server manages the LLM conversation and writes events to files. Clients are separate neovim processes that read those files and render the UI.

```
┌─────────────────────────────────────────────────────────────┐
│  tmux session                                               │
│                                                             │
│  ┌──────────────────┐  ┌─────────────────┐                  │
│  │  Display (nvim)  │  │  Tree (TUI)     │                  │
│  │  Reads JSONL     │  │  Subagent/tool  │                  │
│  │  Renders chat    │  │  hierarchy      │                  │
│  ├──────────────────┤  ├─────────────────┤                  │
│  │  Edit (nvim)     │  │  Permission     │                  │
│  │  Writes messages │  │  (TUI)          │                  │
│  │  via Unix socket │  │  Pending/granted│                  │
│  └────────┬─────────┘  └────────┬────────┘                  │
│           │ display.jsonl       │ server.sock                │
│           └────────┬────────────┘                            │
│                    ▼                                         │
│           Server Process                                     │
│           ├─ ConversationManager                            │
│           ├─ PermissionManager                              │
│           ├─ JSONL event writer                              │
│           └─ Unix socket listener                           │
│                                                             │
│  Session dir: ~/.tcode/sessions/{id}/                       │
└─────────────────────────────────────────────────────────────┘
```

## Configuration

All settings are configured via TOML config files in `~/.tcode/`:

| File | Purpose |
|------|---------|
| `config.toml` | Default config |
| `config-<profile>.toml` | Profile-specific config (selected with `-p <profile>`) |

On first run, tcode auto-creates `~/.tcode/config.toml` with all options commented out as a template (0600 permissions). Profiles are fully self-contained — there is no inheritance from the default config. Missing profile = error. Missing fields use built-in defaults.

### CLI Flags

```bash
tcode                        # Start with default config
tcode -p work                # Start with ~/.tcode/config-work.toml
tcode --session <id> attach  # Attach to existing session
```

The CLI only accepts `--session` and `-p`/`--profile`. All other settings live in the config file.

### Config File Reference

```toml
# ~/.tcode/config.toml

provider = "claude"              # claude | open-ai | open-router
api_key = ""                     # or set env var (see table below)
model = "claude-opus-4-6"        # defaults per provider
base_url = ""                    # defaults per provider
subagent_max_iterations = 50
max_subagent_depth = 10
subagent_model_selection = false
browser_server_url = ""          # remote browser-server URL (TCP mode)
browser_server_token = ""        # bearer token for remote browser-server
search_engine = "kagi"           # kagi | google
```

### Providers

| Provider | Config value | Env Variable | Default Model | Default Base URL |
|----------|-------------|--------------|---------------|------------------|
| Claude | `claude` | `ANTHROPIC_API_KEY` | `claude-opus-4-6` | `https://api.anthropic.com` |
| OpenAI | `open-ai` | `OPENAI_API_KEY` | `gpt-5-nano` | `https://api.openai.com/v1` |
| OpenRouter | `open-router` | `OPENROUTER_API_KEY` | `deepseek/deepseek-r1` | `https://openrouter.ai/api/v1` |

If `api_key` is not set in the config, the provider's environment variable is used.

### Layout Configuration

The tmux pane layout is configured as a binary split tree. Each node is either a `split` (with two children `a` and `b`) or a `command` (a leaf pane). Sizes are percentages and siblings must add up to 100.

The default layout (used when `[layout]` is omitted):

```toml
[layout]
split = "horizontal"

  [layout.a]
  split = "vertical"
  size = 70

    [layout.a.a]
    command = "display"
    size = 70

    [layout.a.b]
    command = "edit"
    size = 30
    focus = true

  [layout.b]
  split = "vertical"
  size = 30

    [layout.b.a]
    command = "tree"
    size = 50

    [layout.b.b]
    command = "permission"
    size = 50
```

Split directions: `horizontal` (left/right) or `vertical` (top/bottom). Available commands: `display`, `edit`, `tree`, `permission`. A layout must have exactly one `display`, at least one `edit`, and at most one `focus = true`.

### Browser Server

By default, tcode automatically manages a local `browser-server` process via Unix socket at `~/.tcode/browser-server.sock`. Multiple tcode sessions share the same server, and it exits on its own after 5 minutes of inactivity.

To use a remote browser-server instead, set `browser_server_url` and `browser_server_token` in your config file.

## Commands

### `tcode [-p <profile>]`

Starts the server and opens display + edit panes in the current tmux session. Generates a unique 8-character session ID (e.g., `abc12def`) and prints it on startup. Session files persist in `~/.tcode/sessions/{id}/`. Use `-p` to load a specific config profile.

### `tcode [-p <profile>] [--session <id>] attach`

Attaches to an existing session and resumes the conversation in the current tmux session. If `--session` is omitted, an interactive picker lets you select from available sessions. Must be run inside tmux.

### `tcode sessions`

Lists all sessions with their status (active/inactive). Active sessions have a running server.

### `tcode --session <id> serve`

Starts just the server process (no tmux integration). Requires `--session` flag. Reads provider/model/search settings from the config file.

### `tcode --session <id> edit`

Opens a neovim editor for composing and sending messages to the server. Watches `edit-msg.txt` for changes and sends content over the Unix socket.

### `tcode --session <id> display`

Opens a neovim buffer that renders the conversation by tailing `display.jsonl`. Shows user messages, assistant responses, tool calls, and token usage with syntax highlighting.

### `tcode --session <id> tool-call <tool-call-id>`

Opens a neovim buffer showing the detailed output of a specific tool execution. Reads from per-tool-call JSONL files.

### `tcode --session <id> cancel-tool <tool-call-id>`

Cancels a running tool call. Connects to the server's Unix socket, sends a `CancelTool` message, and prints the result. Used by the display pane's `Ctrl-k` keybinding. Works across all conversations (root and subagents) — the `--session` can be a subagent session ID; the root session's socket is resolved automatically.

### `tcode --session <id> cancel-conversation <conversation-id>`

Cancels an entire conversation, cascading to all running tools and child subagents. The server looks up the conversation via `ConversationManager`, calls `ConversationClient::cancel()`, which recursively cancels the conversation's cancel token (and all child tool tokens), all registered child subagent conversations, and broadcasts a system warning. The `--session` can be a subagent session ID; the root session's socket is resolved automatically.

### `tcode --session <id> tree`

Opens a TUI tree view of the conversation's subagents and tool calls. Displays status, token usage, and hierarchical nesting. Automatically shown as a right pane when starting a new session.

| Key | Action |
|-----|--------|
| `j`/`↓` | Move down |
| `k`/`↑` | Move up |
| `Space` | Toggle collapse/expand |
| `Enter`/`o` | Open detail in new tmux window |
| `Ctrl-k` | Cancel selected subagent (running or idle) |
| `f` | Toggle filter (running only / all) |
| `R` | Full refresh |
| `q` | Quit |

### `tcode --session <id> permission`

Opens a TUI pane showing all tool permissions — pending requests, session grants, and project grants — grouped by tool and key. All known scopes and keys are always shown as a skeleton tree (from the `ALL_SCOPES` registry), even when no permissions have been requested yet. Automatically shown as a bottom-right pane when starting a new session.

The pane watches `display.jsonl` for `PermissionUpdated` signals and queries the server for the latest permission state over the Unix socket.

| Key | Action |
|-----|--------|
| `j`/`↓` | Move down |
| `k`/`↑` | Move up |
| `Space` | Toggle collapse/expand |
| `Enter`/`o` on pending permission | Open approval popup |
| `Enter`/`o` on granted permission | Open management popup (revoke) |
| `Enter`/`o` on key node | Open add-permission popup |
| `f` | Toggle filter (pending only / all) |
| `R` | Full refresh |
| `q` | Quit |

Pressing `Enter`/`o` on a pending permission opens a `tmux display-popup` with approval options. Pressing `Enter`/`o` on a granted permission opens a management popup to revoke it. Pressing `Enter`/`o` on a key node opens an add-permission popup where the user can type a value and choose session or project scope to proactively grant a permission.

### `tcode --session <id> approve-next`

Opens pending tool approvals one by one in tmux popups. Each approval is fetched from the server via `GetPermissionState`. Prints "No pending approvals" if none exist; exits silently if the user cancels a popup. Used by the `Ctrl-p` keybinding in display and edit windows.

### `tcode --session <id> approve --tool <t> --key <k> --value <v> [--manage]`

Opens a small approval dialog (designed for `tmux display-popup`). Shows the permission details and presents options:

**Approval mode** (pending permissions):
| Key | Action |
|-----|--------|
| `1` | Allow once |
| `2` | Allow for session |
| `3` | Allow for project (persisted) |
| `4` | Deny |
| `q`/`Esc` | Cancel |

**Management mode** (`--manage`, granted permissions):
| Key | Action |
|-----|--------|
| `r` | Revoke permission |
| `q`/`Esc` | Cancel |

### `tcode --session <id> approve --add --tool <t> --key <k>`

Opens a two-phase add-permission dialog for proactively granting a permission (no pending request needed).

**Phase 1** — type the permission value (e.g., a file path or hostname):
| Key | Action |
|-----|--------|
| printable chars | Append to value input |
| `Backspace` | Delete last character |
| `Enter` | Confirm value, proceed to phase 2 |
| `Esc`/`Ctrl-C` | Cancel |

**Phase 2** — choose scope:
| Key | Action |
|-----|--------|
| `2` | Allow for session |
| `3` | Allow for project (persisted) |
| `Backspace` | Go back to edit value |
| `q`/`Esc`/`Ctrl-C` | Cancel |

### `tcode browser`

Launches Chrome with the persistent profile at `~/.tcode/chrome/`. Use this to log in to services (e.g., Kagi for web search) that the browser-server needs. Google search works without authentication. This is a standalone command that opens a visible Chrome window — it does not interact with the browser-server process.

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
| `tool-call-{id}-status.txt` | Per-tool-call status (`Running`, `Permission`, `Done`, `Failed`, `Cancelled`, `Denied`, `Timeout`) |
| `subagent-{conv_id}/` | Sub-session directory for a subagent (same file structure as parent) |
| `permissions.json` | Project-level tool permissions (persisted across sessions) |
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
| `Ctrl-k` | Running subagent | Cancel the subagent conversation (with confirmation popup) |
| `Ctrl-k` | Running tool call | Cancel the tool call (with confirmation popup) |
| `Ctrl-c` | Anywhere | Cancel the entire conversation (with confirmation popup) |
| `Ctrl-p` | Anywhere | Open pending tool approvals one by one |
| `q` | Anywhere | Quit |

Context is determined by cursor position using extmarks. `Ctrl-k` checks for a subagent under the cursor first, then falls back to tool call. A `[Ctrl-k to cancel]` hint is shown on tool/subagent labels while they are running and removed when they finish. `Ctrl-c` reads the root conversation ID from `conversation-state.json` and cancels it, cascading to all running tools and child subagents. `Ctrl-p` calls `tcode approve-next` to loop through pending tool approvals via tmux popups.

## Edit Keybindings

| Key | Mode | Action |
|-----|------|--------|
| `Enter` | Insert | Send message |
| `Ctrl-s` | Normal | Send message |
| `Ctrl-j` | Insert | Insert newline at cursor |
| `Ctrl-p` | Normal/Insert | Open pending tool approvals one by one |
| `Tab` | Insert | Expand `/shortcut`, show completion popup, or insert tab |

## Shortcut Templates

Type `/shortcutname` and press `Tab` in the edit buffer to expand pre-configured prompt templates. Shortcuts work anywhere in the text — typing `/` after a space (or at the start of a line) automatically shows the completion popup.

### Setup

Copy the default shortcuts config to your tcode config directory:

```bash
mkdir -p ~/.tcode
cp tcode/config/shortcuts.lua ~/.tcode/shortcuts.lua
```

Or if tcode is already installed, the install script copies it automatically (won't overwrite existing).

### Usage

| Input | Result |
|-------|--------|
| `/` | Auto-shows completion popup with all shortcuts |
| `/rev` + Tab | Shows completion popup filtered to matches (`/review`, `/refactor`) |
| `/review` + Tab | Expands to template text (exact match) |
| `please /review this` + Tab | Expands `/review` in place, keeps surrounding text |
| `hello world` + Tab | Inserts a normal tab (no `/` nearby) |

Select from the popup with Enter — the shortcut auto-expands after selection.

Edit `~/.tcode/shortcuts.lua` to add your own shortcuts:

```lua
return {
  review = [[Review this code for correctness and edge cases.]],
  explain = "Explain this code in detail.",
  ["my-shortcut"] = [[Use ["quoted-key"] syntax for names with hyphens.]],
}
```

## Cancellation

### Tool Cancellation

The server maintains a shared map of `tool_call_id -> ConversationClient`, populated by event writers as they process `ToolMessageStart`/`ToolMessageEnd` events. This allows cancellation to work across all conversations (root and subagents) without the display needing to know which conversation owns a tool call.

Flow: `Ctrl-k` in display → confirmation popup → `tcode cancel-tool <id>` CLI → Unix socket → server looks up owning `ConversationClient` → `CancellationToken::cancel()` → tool stream aborted.

### Conversation Cancellation

Conversations can be cancelled at the conversation level, which cascades to all running tools and child subagents. Each `ConversationClient` holds a conversation-level `CancellationToken`; individual tool tokens are children of this token, so cancelling the conversation automatically cancels all tools. Child subagent clients are tracked in a `children` map and recursively cancelled.

**Cancelling a conversation:**
Flow: `Ctrl-c` in display (or `Ctrl-k` on a subagent) → confirmation popup → `tcode cancel-conversation <id>` CLI → Unix socket → server looks up `ConversationClient` via `ConversationManager::get_conversation()` → `ConversationClient::cancel()` → cascades to all tools and child subagents.

**After cancellation:**
- The LLM streaming loop detects the cancelled token and broadcasts `AssistantMessageEnd(Cancelled)`.
- Any pending tool calls that were interrupted get `ToolMessageEnd(Cancelled)` or `SubAgentTurnEnd(Cancelled)` messages.
- The cancel token is then reset so the conversation can accept new messages (subagents can be resumed via `continue_subagent`).
- When a child subagent is cancelled, the parent's `user_interrupted` flag is set, causing the parent's `call_llm` loop to break and return to waiting for user input rather than auto-continuing with tool results.
- Cancelled subagent results include a message telling the LLM not to retry unless the user explicitly asks.

## Neovim Plugin

`lua/tcode.lua` is the neovim plugin that powers the display and edit clients. It handles:

- JSONL event parsing and incremental rendering into neovim buffers
- Syntax highlighting (TCodeUser, TCodeAssistant, TCodeTool, TCodeError, TCodeTokens)
- Auto-scrolling (only when cursor is at bottom)
- File watching via inotify
- Token usage display

### Plugin Compatibility

TCode windows use custom statuslines to show connection status, token usage, and keybinding hints. On setup, TCode automatically disables conflicting plugins in its neovim instances:

- **Statusline plugins** (lualine, vim-airline, lightline) — disabled and their autocmds removed so they cannot re-assert
- **Dashboard/start screen plugins** (alpha-nvim, dashboard-nvim, snacks.nvim dashboard, mini.starter) — their buffers are wiped on setup

This only affects TCode's own neovim processes — your other neovim instances are not affected.

### Syntax Highlighting (Tree-Sitter)

TCode includes a custom tree-sitter grammar (`tree-sitter-tcode`) that parses the display buffer. The grammar splits content at separator lines (the `►` delimiters between messages) and injects each content region as **markdown**, so Neovim's built-in markdown tree-sitter highlighting works inside tcode buffers.

**How it works:**
- The external scanner (`scanner.c`) recognizes `►` lines as separators and everything between them as content blocks.
- `highlights.scm` styles the separator lines.
- `injections.scm` tells Neovim to parse each content block as markdown.
- At runtime, tcode loads the parser via `vim.treesitter.language.add()` and writes the query files into the session directory.

**Installation:** `install.sh` compiles the shared library (`libtree-sitter-tcode.so` / `.dylib`) via `make` and copies it to `/usr/lib`. The parser is then found automatically by the tcode binary at startup.

**Markdown rendering with render-markdown.nvim:** Since tcode buffers are injected as markdown, plugins like [render-markdown.nvim](https://github.com/MeanderingProgrammer/render-markdown.nvim) can render headings, code blocks, lists, etc. inside the display pane. Add `tcode` to its file types:

```lua
return {
  {
    "MeanderingProgrammer/render-markdown.nvim",
    ft = { "markdown", "tcode" },
    opts = {
      file_types = { "markdown", "tcode" },
    },
  },
}
```

## Design Notes

The following are design directions, some implemented and some aspirational:

### Message Branching

TCode uses neovim's extmarks to track message boundaries, enabling conversation branching - navigate to a message and branch from there to explore alternative responses.

### Configurable External Tools

The architecture supports configuring different external tools for viewing output, editing, diffs, etc. - anything that can accept tcode's file paths and socket as parameters.

### Diff Viewing

Planned: A `tcode diff` command for reviewing proposed code changes using vimdiff (or a configurable diff tool).
