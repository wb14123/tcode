# tcode

This document covers tcode's internal architecture. For user documentation, see the [docs/](../docs/) directory.

## User Documentation

- [Getting Started](../docs/01-getting-started.md)
- [Configuration](../docs/02-configuration.md)
- [Commands](../docs/03-commands.md)
- [Keybindings](../docs/04-keybindings.md)
- [Neovim Setup](../docs/05-neovim.md)
- [Browser Setup](../docs/06-browser.md)

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

## Cancellation

### Tool Cancellation

The server maintains a shared map of `tool_call_id -> ConversationClient`, populated by event writers as they process `ToolMessageStart`/`ToolMessageEnd` events. This allows cancellation to work across all conversations (root and subagents) without the display needing to know which conversation owns a tool call.

Flow: `Ctrl-k` in display -> confirmation popup -> `tcode cancel-tool <id>` CLI -> Unix socket -> server looks up owning `ConversationClient` -> `CancellationToken::cancel()` -> tool stream aborted.

### Conversation Cancellation

Conversations can be cancelled at the conversation level, which cascades to all running tools and child subagents. Each `ConversationClient` holds a conversation-level `CancellationToken`; individual tool tokens are children of this token, so cancelling the conversation automatically cancels all tools. Child subagent clients are tracked in a `children` map and recursively cancelled.

**Cancelling a conversation:**
Flow: `Ctrl-c` in display (or `Ctrl-k` on a subagent) -> confirmation popup -> `tcode cancel-conversation <id>` CLI -> Unix socket -> server looks up `ConversationClient` via `ConversationManager::get_conversation()` -> `ConversationClient::cancel()` -> cascades to all tools and child subagents.

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

## Design Notes

The following are design directions, some implemented and some aspirational:

### Message Branching

TCode uses neovim's extmarks to track message boundaries, enabling conversation branching - navigate to a message and branch from there to explore alternative responses.

### Configurable External Tools

The architecture supports configuring different external tools for viewing output, editing, diffs, etc. - anything that can accept tcode's file paths and socket as parameters.

### Diff Viewing

Planned: A `tcode diff` command for reviewing proposed code changes using vimdiff (or a configurable diff tool).
