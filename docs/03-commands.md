# tcode CLI Command Reference

## User-Facing Commands

### `tcode`

Starts a new session. Launches the server and opens display, edit, tree, and permission panes in the current tmux session. A unique 8-character session ID is generated automatically. Session files persist in `~/.tcode/sessions/{id}/`.

If no config file exists at `~/.tcode/config.toml`, `tcode` automatically launches the `tcode config` wizard in interactive terminals, writes the file, and exits — run `tcode` again afterward to start a session. In non-interactive contexts (CI, piped stdin), tcode instead exits with a "config not found" error that tells you to run `tcode config`.

```
tcode
tcode -p <profile>
tcode -c <container-name>
tcode -c <container-name> --container-runtime podman
```

**Flags:**

| Flag | Description |
|------|-------------|
| `-p <profile>` | Load a specific config profile |
| `-V`, `--version` | Print version and git commit |
| `-c <name>`, `--container <name>` | Run bash commands inside a running Docker/Podman container. File tools remain on the host. See [02-configuration.md](02-configuration.md#container-mode). |
| `--container-runtime <runtime>` | Container runtime CLI: `docker` (default) or `podman`. Requires `-c`. |

---

### `tcode config`

Interactively creates a new tcode config file at `~/.tcode/config.toml` (or `~/.tcode/config-<profile>.toml` with `-p`). Prompts for `provider`, `base_url`, and `api_key` and writes the result with all other options (`model`, layout, shortcuts, subagent limits, browser server, search engine) left as commented-out lines for you to uncomment later.

```
tcode config
tcode -p <profile> config
```

**Flags:**

| Flag | Description |
|------|-------------|
| `-p <profile>` | Write to `~/.tcode/config-<profile>.toml` instead of the default file |

**Behavior:**

- **Provider choices.** The wizard menu offers five options: `claude` (Anthropic API key), `claude-oauth` (Claude Pro/Max subscription via OAuth), `open-ai` (OpenAI API key), `open-ai-oauth` (OpenAI Codex/ChatGPT Pro subscription via OAuth), and `open-router` (OpenRouter API key). The two OAuth providers (`claude-oauth`, `open-ai-oauth`) are distinct provider values: the wizard skips both the base URL and API-key prompts, writes the corresponding `provider = "..."` to the config file, and tells you to run the matching auth command (`tcode claude-auth` or `tcode openai-auth`, with `-p <profile>` as needed) afterward. At runtime, OAuth providers load tokens from disk using the selected profile and ignore both `api_key` in the config and the provider's environment variable.
- **Refuses to overwrite.** If the target file already exists, the wizard errors with ``Config already exists at <path>. Edit it directly, or delete it first and re-run `tcode config`.`` To regenerate, delete the file first and re-run the wizard.
- **File permissions.** On Unix the file is written with `0600` permissions via a temp-file + rename dance, so a crash or Ctrl-C mid-wizard does not leave a partial file at the real path.
- **Next-steps output.** After writing, the wizard prints the config file's absolute path and points at [02-configuration.md](02-configuration.md) for the full reference. For `claude-oauth`, it also prints a reminder to run the matching `claude-auth` command for the selected profile; for `open-ai-oauth`, a reminder to run the matching `openai-auth` command for the selected profile.

See [02-configuration.md](02-configuration.md#config-file-location) for the wizard's first-run auto-launch behavior and the full list of options you can uncomment later.

---

### `tcode attach`

Attaches to an existing session and resumes the conversation in the current tmux session. Must be run inside tmux. If `--session` is omitted, an interactive picker is shown.

```
tcode attach
tcode --session <id> attach
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | Session ID to attach to. If omitted, an interactive picker is shown. |

---

### `tcode sessions`

Lists all sessions with their status (active or inactive). Active sessions have a running server process.

```
tcode sessions
```

---

### `tcode tree`

Opens a TUI tree view of the conversation's subagents and tool calls. Displays status, token usage, and hierarchical nesting. This pane is automatically shown in the right column when starting a new session.

```
tcode tree
tcode --session <id> tree
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | Session ID. If omitted, an interactive picker is shown. |

---

### `tcode permission`

Opens a TUI pane showing all tool permissions: pending requests, session grants, and project grants, grouped by tool and key. All known scopes and keys are always shown as a skeleton tree.

```
tcode permission
tcode --session <id> permission
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | Session ID. If omitted, an interactive picker is shown. |

---

### `tcode browser`

Launches Chrome with the persistent profile at `~/.tcode/chrome/`. Use this to log in to services (e.g., Kagi for web search). This is a standalone command and does not interact with the browser-server process. Press Ctrl+C to exit when done.

```
tcode browser
```

---

### `tcode claude-auth`

Authenticates with Claude via OAuth. Intended for Claude Pro/Max subscribers who want to use their subscription credits via the API. Opens an authorization URL in the browser; the user pastes the returned code back into the terminal. On success, saves tokens to the profile-aware Claude token file: `~/.tcode/auth/claude_tokens.json` with no profile, or `~/.tcode/auth/claude_tokens-<profile>.json` when run with `-p <profile>`.

```
tcode claude-auth
tcode -p <profile> claude-auth
```

The same profile must be used later at runtime; `tcode -p <profile> ...` loads the matching `claude_tokens-<profile>.json` file and does not fall back to the default token file.

---

### `tcode openai-auth`

Authenticates with OpenAI via OAuth. Intended for OpenAI Codex / ChatGPT Pro subscribers who want to use their subscription via the API. Starts a local HTTP server on port 1455 and opens the browser for login (PKCE authorization code flow). On success, saves tokens to the profile-aware OpenAI token file: `~/.tcode/auth/openai_tokens.json` with no profile, or `~/.tcode/auth/openai_tokens-<profile>.json` when run with `-p <profile>`.

```
tcode openai-auth
tcode -p <profile> openai-auth
```

The same profile must be used later at runtime; `tcode -p <profile> ...` loads the matching `openai_tokens-<profile>.json` file and does not fall back to the default token file.

---

## Internal / Plumbing Commands

These commands are invoked internally by tcode -- from display keybindings, tmux popups, or the server process. They are not intended for direct use but are documented here for completeness.

---

### `tcode serve`

Starts just the server process without any tmux integration.

```
tcode --session <id> serve
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID |

---

### `tcode edit`

Opens a neovim editor for composing messages to send to the conversation.

```
tcode --session <id> edit
tcode --session <id> edit --conversation-id <cid>
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID |
| `--conversation-id <id>` | Target a specific conversation (optional) |

---

### `tcode display`

Opens a neovim buffer that renders the conversation by tailing `display.jsonl`.

```
tcode --session <id> display
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID |

---

### `tcode tool-call`

Opens a neovim buffer showing detailed output of a specific tool execution.

```
tcode --session <id> tool-call <tool-call-id>
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID |

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<tool-call-id>` | **(required)** The ID of the tool call to inspect |

---

### `tcode cancel-tool`

Cancels a running tool call. The `--session` value can be a subagent session ID; the root session's socket is resolved automatically.

```
tcode --session <id> cancel-tool <tool-call-id>
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID (can be a subagent session) |

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<tool-call-id>` | **(required)** The ID of the tool call to cancel |

---

### `tcode cancel-conversation`

Cancels an entire conversation, cascading cancellation to all running tools and child subagents. The `--session` value can be a subagent session ID; the root session's socket is resolved automatically.

```
tcode --session <id> cancel-conversation <conversation-id>
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID (can be a subagent session) |

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<conversation-id>` | **(required)** The ID of the conversation to cancel |

---

### `tcode open-tool-call`

Opens a tool-call detail view in a new tmux window.

```
tcode --session <id> open-tool-call <tool-call-id>
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID |

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<tool-call-id>` | **(required)** The ID of the tool call to open |

---

### `tcode open-subagent`

Opens a subagent's display and edit panes in a new tmux window (split layout).

```
tcode --session <id> open-subagent <conversation-id>
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID |

**Arguments:**

| Argument | Description |
|----------|-------------|
| `<conversation-id>` | **(required)** The conversation ID of the subagent to open |

---

### `tcode approve-next`

Opens pending tool approval requests one by one in tmux popups. This is the handler behind the `Ctrl-p` keybinding.

```
tcode --session <id> approve-next
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID |

---

### `tcode approve`

Opens an approval or management dialog, designed to run inside `tmux display-popup`. Supports three modes: approve (default), management/revoke, and add-permission.

```
tcode --session <id> approve --tool <t> --key <k> --value <v>
tcode --session <id> approve --tool <t> --key <k> --value <v> --once-only
tcode --session <id> approve --tool <t> --key <k> --manage
tcode --session <id> approve --tool <t> --key <k> --add
```

**Flags:**

| Flag | Description |
|------|-------------|
| `--session <id>` | **(required)** Session ID |
| `--tool <t>` | **(required)** Tool name |
| `--key <k>` | **(required)** Permission key |
| `--value <v>` | Permission value (required unless `--add`) |
| `--manage` | Open management/revoke mode instead of approve mode |
| `--add` | Add-permission mode with interactive value input (conflicts with `--manage`) |
| `--prompt <str>` | Human-readable prompt text (default: "") |
| `--request-id <uuid>` | Per-invocation request ID for AllowOnce targeting |
| `--preview-file-path <path>` | File path to preview (enables "[v] View in nvim") |
| `--once-only` | Only offer "Allow once" and "Deny" (no session/project caching) |

**Keybindings by mode:**

Approve mode -- Phase 1 (menu):

| Key | Action |
|-----|--------|
| `1` | Allow once |
| `2` | Allow for session |
| `3` | Allow for project |
| `4` | Deny (proceed to Phase 2 for optional reason) |
| `q` / `Esc` | Cancel |

Approve mode -- Phase 2 (deny reason, only when `4` was chosen):

| Key | Action |
|-----|--------|
| Printable chars | Append to reason input (500 char max) |
| `Backspace` (non-empty) | Delete last character |
| `Backspace` (empty) | Go back to Phase 1 |
| `Enter` | Deny — empty / whitespace-only input denies without a reason |
| `Esc` | Go back to Phase 1 |
| `Ctrl-C` | Cancel the popup |

Management mode:

| Key | Action |
|-----|--------|
| `r` | Revoke |
| `q` / `Esc` | Cancel |

Add mode -- Phase 1 (type value):

| Key | Action |
|-----|--------|
| Printable chars | Input value |
| `Backspace` | Delete character |
| `Enter` | Confirm value |
| `Esc` / `Ctrl-C` | Cancel |

Add mode -- Phase 2 (choose scope):

| Key | Action |
|-----|--------|
| `2` | Grant for session |
| `3` | Grant for project |
| `Backspace` | Go back to Phase 1 |
| `q` / `Esc` / `Ctrl-C` | Cancel |
