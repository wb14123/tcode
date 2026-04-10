# Getting Started

This guide walks you through installing tcode and running it for the first time.

## Prerequisites

- **Operating system** -- Linux or macOS. Windows is not supported natively, but tcode works on WSL2 (Chrome browser tools may require extra library dependencies; see [06-browser.md](06-browser.md)).

- **tmux** -- tcode manages its UI as panes inside a tmux session. Install via your system package manager (`apt install tmux`, `brew install tmux`, etc.).

- **Neovim** (>= 0.9) -- tcode uses neovim for its display and edit windows. The [LazyVim](https://www.lazyvim.org/) distribution is recommended. See [05-neovim.md](05-neovim.md) for plugin setup.

- **Chrome or Chromium** (optional) -- required only if you want to use the `web_search` and `web_fetch` tools. Run `tcode browser` to configure. See [06-browser.md](06-browser.md) for details.

## Installation

### Install from binary release (recommended)

```sh
curl -sSL https://raw.githubusercontent.com/wb14123/tcode/refs/heads/master/install.sh | sh
```

This downloads the latest release and installs:
- `tcode` and `browser-server` to `/usr/local/bin`
- `libtree-sitter-tcode.so` (or `.dylib` on macOS) to `/usr/local/lib`

To install a specific version:

```sh
curl -sSL https://raw.githubusercontent.com/wb14123/tcode/refs/heads/master/install.sh | VERSION=v0.2.0 sh
```

#### User-local install (no sudo)

If you don't have root, or prefer a self-contained install under your home directory, pass `--user`:

```sh
curl -sSL https://raw.githubusercontent.com/wb14123/tcode/refs/heads/master/install.sh | sh -s -- --user
```

This installs to `~/.local/bin` and `~/.local/lib` and never invokes `sudo`. Make sure `~/.local/bin` is on your `$PATH`; the installer prints a shell-specific hint at the end if it isn't. The `VERSION` environment variable also works with `--user`:

```sh
curl -sSL https://raw.githubusercontent.com/wb14123/tcode/refs/heads/master/install.sh | VERSION=v0.2.0 sh -s -- --user
```

> **macOS manual download:** If you download `.tar.gz` from GitHub Releases via a browser instead of using the install script, macOS may block the binaries. Run `xattr -d com.apple.quarantine` against the installed files to fix this. For a system install: `xattr -d com.apple.quarantine /usr/local/bin/tcode /usr/local/bin/browser-server /usr/local/lib/libtree-sitter-tcode.dylib`. For a user install: `xattr -d com.apple.quarantine ~/.local/bin/tcode ~/.local/bin/browser-server ~/.local/lib/libtree-sitter-tcode.dylib`.

### Build from source

Building from source requires:
- **Rust toolchain** — install via [rustup](https://rustup.rs/)
- **A C compiler** — needed to build the tree-sitter grammar (`gcc` or `clang`)
- **Node.js** — required by `tree-sitter generate`
- **tree-sitter CLI** — install via `npm install -g tree-sitter-cli`

```sh
git clone https://github.com/wb14123/tcode.git && cd tcode
./install-from-source.sh          # system install to /usr/local (uses sudo)
# or, for a user-local install (no sudo):
./install-from-source.sh --user   # installs to ~/.local
```

`install-from-source.sh` runs `cargo build --release` and then installs `tcode`, `browser-server`, and the tree-sitter shared library into `<prefix>/bin` and `<prefix>/lib`.

To uninstall:

```sh
# System install:
sudo rm /usr/local/bin/tcode /usr/local/bin/browser-server /usr/local/lib/libtree-sitter-tcode.*
# User install:
rm ~/.local/bin/tcode ~/.local/bin/browser-server ~/.local/lib/libtree-sitter-tcode.*
```

## Set up render-markdown (recommended)

tcode uses a custom `tcode` filetype for the display buffer. To get rendered headings, code blocks, lists, and other markdown elements, you need to configure [render-markdown.nvim](https://github.com/MeanderingProgrammer/render-markdown.nvim) to handle this filetype. Add `tcode` to its file types in your neovim config (e.g., as a LazyVim plugin spec):

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

Without this, the display pane will have no syntax highlighting for markdown content. See [05-neovim.md](05-neovim.md) for more neovim setup details.

## First Run

### Configure a provider

> **The agent can read all files under the current directory by default.** tcode automatically grants the agent read access to everything inside the directory you launch it from — no approval prompt. Always `cd` into the specific project you want to work on before running `tcode`. Don't launch it from broad directories like `~` or `/`. See [07-permissions.md](07-permissions.md) for details.

Inside a tmux window, `cd` into your project and run:

```sh
tcode
```

On first launch tcode detects that no config file exists and auto-starts the interactive setup wizard. You can re-run the wizard any time with `tcode config`, or with `tcode -p <profile> config` to create a profile-specific config at `~/.tcode/config-<profile>.toml`. The wizard **refuses to overwrite** an existing file — delete it first if you want to regenerate.

Pick a provider from the menu:

- **`claude`** — Anthropic API-key mode. Paste your key at the wizard prompt, or leave it blank; an empty input is written as `api_key = ""` and at runtime tcode falls back to `ANTHROPIC_API_KEY` from your shell if the env var is set.
- **`claude-oauth`** — Claude Pro/Max subscription via OAuth. The wizard skips both the base URL and API-key prompts; after it finishes, run `tcode claude-auth` to authenticate. This provider loads tokens via `tcode claude-auth` and ignores both the config `api_key` and `$ANTHROPIC_API_KEY` entirely.
- **`open-ai`** — OpenAI API-key mode. Paste your key, or leave it blank to fall back to `OPENAI_API_KEY` at runtime.
- **`open-router`** — OpenRouter API-key mode. Paste your key, or leave it blank to fall back to `OPENROUTER_API_KEY` at runtime.

| Provider      | Environment Variable   |
|---------------|------------------------|
| `claude`      | `ANTHROPIC_API_KEY`    |
| `open-ai`     | `OPENAI_API_KEY`       |
| `open-router` | `OPENROUTER_API_KEY`   |

`claude-oauth` has no environment variable — it authenticates via OAuth only.

Accept the default base URL or override it, then paste your API key (or leave it blank to fall back to the env var). For `claude-oauth`, both the base URL and API-key prompts are skipped entirely.

The wizard writes the config to `~/.tcode/config.toml` (or `~/.tcode/config-<profile>.toml` with `-p`), prints the absolute path, and exits. Run `tcode` again to start your first session.

**Typical first-time flow for `claude-oauth`:**

1. `tcode` — wizard runs, pick `claude-oauth`, accept defaults, wizard exits.
2. `tcode claude-auth` — complete OAuth in the browser.
3. `tcode` — launches the full four-pane UI.

All other options (model, layout, shortcuts, subagent limits, browser server, search engine) live as commented-out lines in the generated file. Open `~/.tcode/config.toml` in your editor to uncomment and tune them. See [02-configuration.md](02-configuration.md) for the full reference.

### The four-pane layout

tcode opens a four-pane layout inside your tmux window:

```
+------------------+------------------+
|                  |                  |
|     display      |      tree        |
|  (conversation)  | (subagent/tool   |
|                  |    hierarchy)    |
+------------------+------------------+
|                  |                  |
|      edit        |   permission     |
| (compose message)| (tool approvals) |
|                  |                  |
+------------------+------------------+
```

- **display** (top-left) -- shows the conversation history. This is a neovim buffer, so all your normal neovim navigation works (scrolling, searching with `/`, etc.).
- **edit** (bottom-left) -- where you compose messages. Also a neovim buffer, so you have full neovim editing (motions, registers, visual mode, etc.).
- **tree** (top-right) -- shows the subagent and tool call hierarchy.
- **permission** (bottom-right) -- where tool permission prompts appear.

**Navigating between panes:** These are tmux panes, so you use tmux keybindings to move between them. The default tmux prefix is `Ctrl-b`:

- `Ctrl-b` then arrow key -- move to the pane in that direction
- `Ctrl-b o` -- cycle to the next pane
- `Ctrl-b z` -- zoom the current pane to full screen (press again to restore)

Most of the time you'll stay in the edit pane to type messages and use `Ctrl-p` to handle permissions without switching panes.

### Send your first message

Focus starts in the edit pane. Type your message, then:

- Press **Enter** in insert mode to send, or
- Press **Ctrl-s** in normal mode to send.

Use **Ctrl-j** in insert mode to insert a newline without sending.

The agent's response will stream into the display pane.

### Approving tool permissions

When the agent tries to use a tool that requires permission (e.g., reading a file, running a command, or fetching a URL), a permission request appears in the **permission** pane (bottom-right). You can approve it in two ways:

- **From any pane:** Press **Ctrl-p** to open the next pending approval as a tmux popup. Choose:
  - `1` — Allow once (this invocation only)
  - `2` — Allow for session (until you close tcode)
  - `3` — Allow for project (persisted across sessions)
  - `4` — Deny

- **From the permission pane:** Navigate to a pending request with `j`/`k` and press **Enter** to open the approval popup.

Approved permissions are visible in the permission pane. You can navigate to any granted permission and press **Enter** to revoke it. This gives you a clear view of exactly what the agent can do at any point. See [07-permissions.md](07-permissions.md) for a full explanation of how permissions work (scopes, matching, adding permissions proactively, etc.) and [04-keybindings.md](04-keybindings.md) for the full keybinding reference.

### Using shortcuts

tcode comes with built-in prompt shortcuts that save typing for common workflows. In the edit pane, type `/` followed by a shortcut name and press **Tab** to expand it. For example:

- `/plan` + Tab — expands to a prompt asking the agent to design and plan before implementing
- `/review` + Tab — expands to a prompt asking for a code review via subagent
- `/save-plan` + Tab — asks the agent to save a plan to `plan.md`
- `/implement-plan` + Tab — asks the agent to implement an existing `plan.md`

Typing `/` at the start of a line or after a space shows a completion popup with all available shortcuts. You can customize these in the `[shortcuts]` section of your config file — see [02-configuration.md](02-configuration.md#shortcut-templates) for details.

### Monitoring subagents and tool calls

When the agent spawns subagents or runs tools, they appear in the **tree** pane (top-right) as a live hierarchy. From the tree pane:

- `j`/`k` to navigate, `Space` to expand/collapse
- **Enter** to open a subagent's conversation or tool call detail in a new tmux window
- **Ctrl-k** to cancel a running subagent

From the **display** pane, press **o** on a subagent or tool call block to open its detail in a new window. Press **Ctrl-k** on a running tool/subagent to cancel it, or **Ctrl-c** to cancel the entire conversation.

Detail views open as separate tmux windows. Use tmux window navigation to switch between them and the main tcode instance:

- `Ctrl-b n` / `Ctrl-b p` -- next / previous tmux window
- `Ctrl-b <number>` -- jump to a specific window by index
- **q** -- close the detail view and return to the previous window

### A typical workflow

1. Start tcode in your project directory: `tcode`
2. Type `/plan` + Tab, then describe what you want to build. Send it.
3. The agent designs a plan. Approve any tool permissions it needs (file reads, etc.) via **Ctrl-p**.
4. Review the plan, ask questions or suggest changes.
5. When satisfied, type `/implement-plan` + Tab and send.
6. Monitor progress in the tree pane. Approve permissions as they come up.
7. When done, type `/review` + Tab to have the agent review its own changes.

## Next Steps

- [02-configuration.md](02-configuration.md) -- full config reference
- [03-commands.md](03-commands.md) -- CLI commands and usage
- [04-keybindings.md](04-keybindings.md) -- keyboard shortcuts
- [05-neovim.md](05-neovim.md) -- neovim plugin setup
- [06-browser.md](06-browser.md) -- browser and web tools setup
- [07-permissions.md](07-permissions.md) -- how the permission system works
