# Tool Permission System Design

## Overview

A tool-level permission system via a centralized `PermissionManager` that mediates tool access to sensitive resources.

## Architecture

### PermissionManager

A centralized service that handles permission checking, user prompting, and persistence.

```rust
trait PermissionManager {
    /// Create a scoped handle for a specific tool.
    /// The executor calls this before dispatching to a tool.
    fn for_tool(&self, tool_name: &str) -> ScopedPermissionManager;
}

struct ScopedPermissionManager {
    tool_name: String,
    // ... inner state
}

impl ScopedPermissionManager {
    /// Query stored permissions without prompting the user.
    /// Tools use this to implement custom matching logic (e.g., prefix/wildcard).
    ///
    /// - `key`: Permission category, e.g. "hostname", "command"
    /// - `value`: The exact value to look up
    ///
    /// Returns true if an exact (tool, key, value) match exists in session or project storage.
    fn has_permission(&self, key: &str, value: &str) -> bool;

    /// Check if the action is permitted. Prompts the user if no saved preference exists.
    ///
    /// - `prompt`: Human-readable description, e.g. "Allow web_fetch to access example.com?"
    /// - `key`: Permission category, e.g. "hostname", "command", "path"
    /// - `value`: The specific value being requested, e.g. "example.com", "git", "/etc/passwd"
    ///
    /// Returns true if allowed, false if denied.
    fn ask_permission(&self, prompt: &str, key: &str, value: &str) -> bool;
}
```

### Permission Lookup

Stored permissions are keyed by `(tool, key, value)` tuple.

When `ask_permission` is called:
1. Check if `(tool, key, value)` exists in **project-level** saved permissions -> return allowed
2. Check if `(tool, key, value)` exists in **session-level** saved permissions -> return allowed
3. No saved preference found -> prompt the user

### User Prompt Options

When the user is prompted, they see these options:
1. **Allow once** - Returns `true` for this invocation only. Nothing is persisted.
2. **Allow for session** - Saves `(tool, key, value)` in session memory. Cleared when the session ends.
3. **Allow for project** - Saves `(tool, key, value)` to persistent project storage.
4. **Deny** - Returns `false` for this invocation. Nothing is persisted.

### Responsibility Split: PermissionManager vs Tool

**`PermissionManager` is a UI + storage layer only.** It handles:
- Storing and retrieving `(tool, key, value)` permissions
- Prompting the user via the permission UI
- Persisting user choices (session / project)

**Each tool owns its permission logic.** The tool decides:
- What key/value to check (e.g., hostname, command, path)
- How to interpret stored permissions (e.g., wildcard matching like `git *` for bash)
- When to call `ask_permission` and how many times
- What to do when permission is denied

`ask_permission` is a building block — the tool is responsible for extracting the right parameters from its input, deciding what granularity to check at, and implementing any pattern matching or custom logic on top.

### Tool Integration

Tools do not pass their own name. The executor creates a `ScopedPermissionManager` before dispatching:

```rust
// In the tool executor / dispatcher
let scoped_pm = permission_manager.for_tool("web_fetch");
tool.execute(context_with(scoped_pm)).await;
```

Example: `web_fetch` tool — straightforward exact-match check:

```rust
fn execute(&self, ctx: &ToolContext) -> Result<()> {
    let url = &self.params.url;
    let hostname = url.host_str().unwrap_or("unknown");

    // Simple case: ask_permission handles lookup + prompt in one call
    if !ctx.permission.ask_permission(
        &format!("Allow web_fetch to access {}?", hostname),
        "hostname",
        hostname,
    ) {
        return Err(ToolError::PermissionDenied);
    }

    // ... proceed with fetch
}
```

Example: `bash` tool — custom prefix matching using `has_permission`:

```rust
fn execute(&self, ctx: &ToolContext) -> Result<()> {
    let full_command = &self.params.command; // e.g. "git status --short"
    let first_token = full_command.split_whitespace().next().unwrap_or("");

    // Check if the base command is already approved (without prompting)
    if ctx.permission.has_permission("command", first_token) {
        // "git" is approved -> allow "git status", "git diff", etc.
        return self.run(full_command);
    }

    // No stored permission — prompt the user for the base command
    if !ctx.permission.ask_permission(
        &format!("Allow bash to run `{}`?", first_token),
        "command",
        first_token,
    ) {
        return Err(ToolError::PermissionDenied);
    }

    self.run(full_command)
}
```

Each tool calls `ask_permission` with its own domain-specific key/value:

| Tool       | Key        | Value Example        | Tool-side logic                          |
|------------|------------|----------------------|------------------------------------------|
| web_fetch  | hostname   | `example.com`        | Parses hostname from URL                 |
| bash       | command    | `git`                | Extracts first token; may do prefix match |
| read_file  | path       | `/etc/passwd`        | May check parent directory patterns      |
| write_file | path       | `/home/user/project` | May check parent directory patterns      |

A tool may call `ask_permission` multiple times sequentially if it needs to check multiple things.

### Persistence

- **Session permissions**: In-memory `HashSet<(tool, key, value)>`, cleared on session end.
- **Project permissions**: Stored in a project-local file (location TBD, e.g. `.claude/permissions.json` or similar), keyed by `(tool, key, value)`.

## Data Flow (IPC)

The permission system uses the **existing Unix socket server** (`tcode/src/server.rs`) and event stream — no new IPC mechanism needed.

### New Message Variants (agent → UI via event stream)

Added to `Message` enum, written to `display.jsonl` and picked up by the permission window:

```rust
Message::PermissionRequest {
    msg_id: u64,
    request_id: String,    // unique ID for this permission request
    tool: String,          // tool name
    prompt: String,        // human-readable, e.g. "Allow web_fetch to access example.com?"
    key: String,           // permission category, e.g. "hostname"
    value: String,         // specific value, e.g. "example.com"
}

Message::PermissionResolved {
    msg_id: u64,
    request_id: String,
    allowed: bool,
    scope: PermissionScope, // Once | Session | Project
}
```

### New ClientMessage Variant (UI → agent via socket)

Added to `ClientMessage` enum, sent by `tcode approve` over the Unix socket:

```rust
ClientMessage::ResolvePermission {
    request_id: String,
    decision: PermissionDecision, // AllowOnce | AllowSession | AllowProject | Deny
}
```

### Flow

```
Tool calls ask_permission(prompt, key, value)
  │
  ▼
ScopedPermissionManager checks in-memory/project storage
  │
  ├─ Found → return allowed immediately
  │
  └─ Not found:
       │
       ▼
     Broadcast PermissionRequest on event stream
     (written to display.jsonl, permission window picks it up)
       │
       ▼
     Tool awaits on a tokio::sync::oneshot channel
       │                              ┌──────────────────────────┐
       │                              │  Permission UI window    │
       │                              │  shows pending request   │
       │                              │  user presses Enter/o    │
       │                              │  → tcode approve <id>    │
       │                              │  → floating popup        │
       │                              │  → user picks decision   │
       │                              └──────────┬───────────────┘
       │                                         │
       │                                         ▼
       │                              ClientMessage::ResolvePermission
       │                              sent over Unix socket
       │                                         │
       │                                         ▼
       │                              handle_client_inner receives it,
       │                              saves preference (session/project),
       │                              broadcasts PermissionResolved,
       │◄────────────────────────────── sends result via oneshot
       │
       ▼
     ask_permission returns bool
     Tool continues or aborts
```

### Server-Side Wiring

In `handle_client_inner`, a new match arm alongside `CancelTool`, `CancelConversation`, etc.:

```rust
ClientMessage::ResolvePermission { request_id, decision } => {
    // 1. Save to session/project storage based on decision
    // 2. Broadcast PermissionResolved on event stream (for UI update)
    // 3. Send result through the pending oneshot channel
    // 4. Reply with ServerMessage::Ack
}
```

The `PermissionManager` holds a `HashMap<String, oneshot::Sender<bool>>` of pending requests, keyed by `request_id`. When `ResolvePermission` arrives, it looks up the sender and completes the future.

### Revocation

A separate client message for revoking saved permissions:

```rust
ClientMessage::RevokePermission {
    tool: String,
    key: String,
    value: String,
}
```

On the server side:
- Remove `(tool, key, value)` from session and/or project storage.
- Broadcast a `PermissionRevoked` message on the event stream so the permission UI updates.
- Only affects **future** tool calls — in-flight tools that already passed permission checks are not interrupted.

### Session Resume

When resuming a session (`close_stale_running_items`), any `PermissionRequest` messages that were still pending (no matching `PermissionResolved`) should be closed with a synthetic `PermissionResolved { allowed: false }`. The tools that were awaiting permission would have been killed when the previous session ended, so the oneshot channels no longer exist — this is purely to keep the display state consistent for the permission UI.

### tcode Subcommands

- **`tcode approve <request-id>`** — Thin CLI client. Opens as a floating popup via `tmux display-popup`. Sends `ResolvePermission` over the socket.
- **`tcode approve --manage <permission-id>`** — Same popup pattern, but for managing (revoking) existing permissions. Sends `RevokePermission` over the socket.

## Permission UI

### Layout

The permission approval window is a separate tmux window (`tcode permission`) that opens **to the right of the main edit window** on startup.

The permission list is displayed as a **collapsible tree** grouped by tool → key → values:

```
┌─────────────────────────────┬──────────────────────────────┐
│                             │  tcode permission             │
│   Main edit window          │                              │
│                             │  ▸ web_fetch [-]         ⏳  │
│   [tool call: web_fetch]    │  │  └─ hostname [-]          │
│    status: ⏳ waiting       │  │     ├─ evil.com     ⏳    │
│                             │  │     └─ api.com   (session)│
│                             │  ▸ bash [-]                   │
│                             │  │  └─ command [-]            │
│                             │  │     ├─ curl        ⏳     │
│                             │  │     ├─ git      (project) │
│                             │  │     └─ npm      (session) │
│                             │  ▸ read_file [-]              │
│                             │  │  └─ path [-]               │
│                             │  │     └─ /etc/passwd  ⏳    │
│                             │                              │
└─────────────────────────────┴──────────────────────────────┘
```

**Sort order**: Pending approval items sort first within each group, then alphabetical.

### Permission Window Navigation

Vim-style and arrow key navigation (consistent with `tcode tree`):

| Key                    | Action                                                        |
|------------------------|---------------------------------------------------------------|
| **`j`** / **`↓`**     | Move down                                                     |
| **`k`** / **`↑`**     | Move up                                                       |
| **`o`** / **`Enter`** / **`Space`** | Non-leaf: toggle collapse. Leaf: open approval/manage popup. |
| **`f`**                | Toggle filter: show all / pending only                        |
| **`q`**                | Quit permission window                                        |

### Permission Window Interaction

All dialogs open as **tmux floating popups** (centered on screen via `tmux display-popup`).

**Pending items** — press **`o`**, **`Space`**, or **`Enter`** on a value node:
- Opens `tcode approve <permission-id>` in a centered floating popup.
- Shows full tool call details (tool name, params, prompt).
- Options: Allow once / Allow for session / Allow for project / Deny.

**Approved items** — press **`o`**, **`Space`**, or **`Enter`** on a value node:
- Opens `tcode approve --manage <permission-id>` in a centered floating popup.
- Shows the saved permission details (tool, key, value, scope).
- Options: Revoke.
- Revocation only affects **future** tool calls, not in-flight ones.

### Main Window Interaction

- **`Space`** on a tool call line that is waiting for approval: Jump to / open the approval dialog for that tool call.
- Tool calls that need approval are visually distinct (e.g. highlighted, icon indicator).

### Tool Call States

Tool calls (including those from subagents) have the following states:

| State                       | Display        | Description                                      |
|-----------------------------|----------------|--------------------------------------------------|
| **idle**                    | `⏸ idle`       | Not yet started                                  |
| **waiting_for_permission**  | `⏳ waiting`    | Blocked on user approval in the permission window |
| **running**                 | `▶ running`    | Executing                                        |
| **completed**               | `✓ done`       | Finished successfully                            |
| **failed (denied)**         | `✗ denied`     | User denied the permission                       |
| **failed (error)**          | `✗ error`      | Execution failed for other reasons               |
| **canceled**                | `⊘ canceled`   | Canceled by user or system                       |

### Non-blocking Behavior

- A tool waiting for permission does **not** block other tools or the agent.
- The agent and other independent tool calls continue executing while permission is pending.
- Subagent tool calls also go through the **same centralized permission window** — all approvals are in one place regardless of which agent initiated the call.
- If multiple tools request the **same `(tool, key, value)` permission** concurrently, the requests are **deduplicated** — only one prompt is shown, and all waiting tools receive the same result.

### Permission Window Recovery

If the user closes the permission window:
- A **status bar indicator** appears in the main window showing the count of pending approvals (e.g. `[2 pending permissions]`).
- The status bar hints the user to reopen with a command (e.g. `tcode permission`).

## Open Questions

- Exact file format and location for project-level persistence
- Floating approval dialog layout details
- Bubblewrap (bwrap) for OS-level filesystem sandboxing — consider as an additional layer later
- **Default-allow rules**: Some operations may be low-risk (e.g., reading files within the project directory). Consider whether tools should be able to declare default-allow rules to reduce prompt fatigue. Not needed for v1 but worth revisiting.
