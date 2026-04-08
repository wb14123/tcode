# tcode Keybinding Reference

tcode's display and edit panes are neovim instances, so all your normal neovim keybindings work there (navigation, search with `/`, visual mode, etc.). The panes themselves are managed by tmux, so use your tmux prefix (default `Ctrl-b`) to switch between panes (e.g., `Ctrl-b o` to cycle, `Ctrl-b <arrow>` to move directionally).

The keybindings below are tcode-specific additions on top of neovim and tmux.

## Display View

The display view shows the conversation output (assistant messages, tool calls, thinking blocks, subagent blocks).

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

Context is determined by cursor position using extmarks. `Ctrl-k` checks for a subagent under the cursor first, then falls back to a tool call. A `[Ctrl-k to cancel]` hint is shown on tool and subagent labels while they are running and removed when they finish. `Ctrl-c` reads the root conversation ID and cancels it, cascading to all running tools and child subagents. `Ctrl-p` loops through pending tool approvals via tmux popups.

## Edit View

The edit view is the input area where the user composes messages to send.

| Key | Mode | Action |
|-----|------|--------|
| `Enter` | Insert | Send message |
| `Ctrl-s` | Normal | Send message |
| `Ctrl-j` | Insert | Insert newline at cursor |
| `Ctrl-p` | Normal/Insert | Open pending tool approvals one by one |
| `Tab` | Insert | Expand `/shortcut`, show completion popup, or insert tab |

## Tree View

The tree view shows a hierarchical tree of subagents and tool calls.

| Key | Action |
|-----|--------|
| `j` / `Down` | Move down |
| `k` / `Up` | Move up |
| `Space` | Toggle collapse/expand |
| `Enter` / `o` | Open detail in new tmux window |
| `Ctrl-k` | Cancel selected subagent (running or idle) |
| `f` | Toggle filter (running only / all) |
| `q` | Quit |

## Permission View

The permission view shows tool permissions organized by key, with their current status (pending, granted).

| Key | Action |
|-----|--------|
| `j` / `Down` | Move down |
| `k` / `Up` | Move up |
| `Space` | Toggle collapse/expand |
| `Enter` / `o` on pending permission | Open approval popup |
| `Enter` / `o` on granted permission | Open management popup (revoke) |
| `Enter` / `o` on key node | Open add-permission popup |
| `f` | Toggle filter (pending only / all) |
| `q` | Quit |

Pressing `Enter` or `o` behaves differently depending on what is selected. On a pending permission, it opens a tmux popup with approval options. On a granted permission, it opens a management popup to revoke that permission. On a key node, it opens an add-permission popup where the user can type a value and choose between session or project scope.

## Approval Popup

The approval popup appears as a tmux popup and has several modes depending on context.

### Approval mode (pending permissions)

| Key | Action |
|-----|--------|
| `1` | Allow once |
| `2` | Allow for session |
| `3` | Allow for project (persisted) |
| `4` | Deny |
| `q` / `Esc` | Cancel |

### Management mode (granted permissions)

| Key | Action |
|-----|--------|
| `r` | Revoke permission |
| `q` / `Esc` | Cancel |

### Add-permission mode

**Phase 1 -- enter value:**

| Key | Action |
|-----|--------|
| Printable chars | Append to value input |
| `Backspace` | Delete last character |
| `Enter` | Confirm value, proceed to Phase 2 |
| `Esc` / `Ctrl-C` | Cancel |

**Phase 2 -- choose scope:**

| Key | Action |
|-----|--------|
| `2` | Allow for session |
| `3` | Allow for project (persisted) |
| `Backspace` | Go back to edit value |
| `q` / `Esc` / `Ctrl-C` | Cancel |
