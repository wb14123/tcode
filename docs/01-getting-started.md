# Getting Started

This guide walks you through installing tcode and running it for the first time.

## Prerequisites

- **tmux** -- tcode manages its UI as panes inside a tmux session. Install via your system package manager (`apt install tmux`, `brew install tmux`, etc.).

- **Neovim** (>= 0.9) -- tcode uses neovim for its display and edit windows. The [LazyVim](https://www.lazyvim.org/) distribution is recommended. See [05-neovim.md](05-neovim.md) for plugin setup.

- **Chrome or Chromium** (optional) -- required only if you want to use the `web_search` and `web_fetch` tools. Run `tcode browser` to configure. See [06-browser.md](06-browser.md) for details.

### Build dependencies

There are no pre-built binaries yet, so building from source is currently the only install method. The following are only needed for the build and won't be required once binary releases are available.

- **Rust toolchain** -- install via [rustup](https://rustup.rs/).

- **A C compiler** -- needed to build the tree-sitter grammar (`gcc` or `clang`).

- **tree-sitter CLI** -- needed to generate the parser from the grammar. Install via `npm install -g tree-sitter-cli` or `cargo install tree-sitter-cli`.

## Installation

### Using the install script (recommended)

```sh
git clone <repo-url> && cd llm-rs
./install.sh
```

The install script does the following:

1. Builds the release binaries and tree-sitter grammar (`cargo build --release`)
2. Copies `tcode` and `browser-server` to `/usr/bin`
3. Copies `libtree-sitter-tcode.so` to `/usr/lib`
4. Copies the default `shortcuts.lua` to `~/.tcode/`

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

Create a minimal config file at `~/.tcode/config.toml`:

```toml
[llm]
provider = "claude"
api_key = "sk-ant-..."
```

The default provider is `claude` and the default model is `claude-opus-4-6`. You can omit `api_key` from the config and set it via an environment variable instead:

| Provider      | Environment Variable   |
|---------------|------------------------|
| `claude`      | `ANTHROPIC_API_KEY`    |
| `open-ai`     | `OPENAI_API_KEY`       |
| `open-router` | `OPENROUTER_API_KEY`   |

### Launch tcode

> **The agent can read all files under the current directory by default.** tcode automatically grants the agent read access to everything inside the directory you launch it from — no approval prompt. Always `cd` into the specific project you want to work on before running `tcode`. Don't launch it from broad directories like `~` or `/`. See [07-permissions.md](07-permissions.md) for details.

Open a tmux session and run:

```sh
tcode
```

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

Typing `/` at the start of a line or after a space shows a completion popup with all available shortcuts. You can customize these by editing `~/.tcode/shortcuts.lua` — see [02-configuration.md](02-configuration.md#shortcut-templates) for details.

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
