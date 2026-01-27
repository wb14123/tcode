# TCode

TCode is a coding agent like Claude Code or Codex, running in the terminal. It leverages existing tools like neovim and tmux for better TUI experience instead of creating its own GUI. So that it's more powerful and user has less learning curve if they are already familiar with the existing tools.

## Design

TCode is a server-client design so that you can open many windows with tools like tmux to view different content.

Here are some of the commands:

### tcode

`tcode` will start a server locally (use socket file for proper permission management). It's also the main window which shows the interaction between user and the code agent and the output of the code agent, along with all the other information like status bars. All the content are shown in an embeded neovim buffer for easier viewing.

You can start multiple tcode, but only one per tmux session. The following command will attach to the one that is running in the current tmux session.

### tcode edit

`tcode edit` will create a neovim buffer to send prompt to the tcode. It also handles selections like options provided by tcode agent, accept / reject command and so on.

A `tcode edit` window is started by tcode main window by default, in the same tab, use vertical split.

### tcode details

Show the details in tcode sub agents and too usage. By default the main windows just show something is going on, like exporing the code, calling a tool and so on. This `tcode details` show all those sub tasks.

There is an optional param for `tcode details`, which is only showing one of the sub task. In the main window, there should be a user friendly id for each of the task so that the `tcode details` can use.

By default tcode main window also spawn tmux tabs for each of the task so that the user can navigate to it directly.

### tcode diff

A window monitors the proposed diffs by tcode agent. Only showing if tcode is not set to auto accept edits. It uses the vimdiff tool (configurable) for easier viewing.

By default tcode main window also shows a tmux popup windows (floating window) for the diff if set to not auto accept edits.

## Neovim Streaming

Show output in neovim, example:

```
nvim -c "enew | set ft=sh | call jobstart(['./slow_read.sh', 'slow_read.sh'], {'on_stdout': {j,d,e -> append(line('$')-1, d)}})"
```

## Configurable Extenal Tools

User should be able to config different external tools by providing things like these when given a command parameter:

* view command output
* open `tcode edit`
* open `tcode details`
* open diff
* find tcode instance in the current session

## Message Branching with Extmarks

TCode uses neovim's extmarks to track message boundaries and enable easy conversation branching.

### How It Works

Each message in the conversation buffer is tagged with an extmark containing a unique message ID. Extmarks automatically track positions even as text is inserted or deleted around them.

```
┌─ msg-001 ───────────────────────────────┐
│ User: How do I fix this bug?            │  ← extmark at start
└─────────────────────────────────────────┘
┌─ msg-002 [●] ───────────────────────────┐
│ Agent: Let me check...                  │  ← extmark at start, [●] = branch exists
│ The issue is in foo.rs line 42...       │
└─────────────────────────────────────────┘
┌─ msg-003 ───────────────────────────────┐
│ User: Can you show me?                  │  ← extmark at start
└─────────────────────────────────────────┘
```

### Branching Flow

1. User navigates to a message and presses `<leader>tb` (branch)
2. Neovim plugin finds the extmark at cursor position → gets message ID
3. Plugin sends "branch from msg-id" command to tcode server
4. Server acknowledges, plugin deletes all lines after that message
5. User can now type a new message that becomes the new branch

### Implementation

**Server-side (Rust):**
- Assign unique IDs to each message
- Send message boundaries to neovim via RPC
- Handle `branch_from(msg_id)` requests
- Maintain conversation tree structure

**Neovim plugin (Lua):**
```lua
local ns = vim.api.nvim_create_namespace('tcode_messages')

-- Mark message start
function M.mark_message_start(msg_id, line)
  vim.api.nvim_buf_set_extmark(buf, ns, line, 0, {
    id = msg_id,
    right_gravity = false,
  })
end

-- Find message at cursor
function M.get_message_at_cursor()
  local cursor = vim.api.nvim_win_get_cursor(0)
  local marks = vim.api.nvim_buf_get_extmarks(buf, ns, 0, cursor, {})
  return marks[#marks]  -- last mark before cursor
end

-- Branch from current message
function M.branch_here()
  local msg = M.get_message_at_cursor()
  tcode_rpc.send('branch_from', msg.id)
  local next_mark = get_next_mark(msg.id)
  if next_mark then
    vim.api.nvim_buf_set_lines(buf, next_mark.line, -1, false, {})
  end
end
```

### Keybindings

| Key | Action |
|-----|--------|
| `<leader>tb` | Branch from current message |
| `<leader>te` | Edit current message |
| `<leader>tn` | Navigate to next branch |
| `<leader>tp` | Navigate to previous branch |

### Visual Indicators

- Sign column markers show message boundaries
- `[●]` indicator on messages that have alternate branches
- Virtual text can show message IDs (configurable)

## Tech Stack

* Rust
