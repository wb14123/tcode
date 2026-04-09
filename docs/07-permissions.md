# Permissions

tcode has a permission system that gives you control over what the agent can do — which files it reads and writes, which commands it runs, and which websites it fetches. This page explains how that system works.

> **Best-effort guardrail, not a security boundary.** The permission system is designed to keep the agent in check during normal use — it helps you stay aware of what the agent is doing and catch mistakes before they happen. It is not a sandbox. If you need real security isolation (e.g., running untrusted code, protecting sensitive files outside the project), use proper OS-level tools like Docker containers, VMs, or dedicated user accounts.

## Concepts

Every permission in tcode is described by three fields and a scope:

### Tool

The **tool** (also called "scope" in some parts of the UI) identifies which category of action the permission covers:

| Tool | Description |
|------|-------------|
| `file_read` | Reading files and listing directories |
| `file_write` | Writing, creating, or modifying files |
| `bash` | Running shell commands |
| `web_fetch` | Fetching web pages |

### Key

The **key** says what *type* of resource the permission controls within that tool:

| Tool | Key | Description |
|------|-----|-------------|
| `file_read` | `path` | A directory path — grants read access to everything inside it |
| `file_write` | `path` | A directory path — grants write access to everything inside it |
| `bash` | `command` | A command prefix — grants permission to run commands starting with it |
| `web_fetch` | `hostname` | A hostname — grants permission to fetch pages from that host |

### Value

The **value** is the actual resource being permitted. What it looks like depends on the key:

- **`path`** values are directory paths like `/home/user/myproject`. A file permission always covers the *directory* (and all its children), not a single file. So granting `file_write > path > /home/user/myproject` lets the agent write to any file inside `/home/user/myproject/`.

  `path` permissions gate the dedicated file tools — `read`, `write`, `edit`, `glob`, `grep`, and LSP-backed lookups. Each of those tools consults your grants before touching the filesystem, so for them the restriction is real. It is **not** a filesystem sandbox: the check happens at the tool boundary, not at the OS level. Bash commands are handled separately, by best-effort syntax analysis of the command — see [Bash commands and file paths](#how-matching-works) below.

- **`command`** values are command prefixes like `cargo` or `npm test`. Permissions use prefix matching, so granting `bash > command > cargo` covers `cargo build`, `cargo test`, `cargo clippy`, and so on.

- **`hostname`** values are hostnames like `github.com` or `docs.rs`.

### Scope (lifetime)

The scope controls how long a permission lasts:

| Scope | Lifetime | Stored where |
|-------|----------|--------------|
| **Once** | Single use — gone after this one tool call | Not stored |
| **Session** | Lasts until you close tcode | In memory only |
| **Project** | Persists across sessions for this project directory | On disk |

Project permissions are saved to `~/.tcode/projects/<hash>/permissions.json`, where `<hash>` is derived from the project directory you launched tcode in. This means if you always launch tcode from `/home/user/myproject`, the same permission file is used every time.

## How matching works

Permissions are **hierarchical**:

- **File paths:** A permission for `/home/user/myproject` also covers `/home/user/myproject/src/main.rs` and any other file or subdirectory within it.

- **Commands:** A permission for `cargo` also covers `cargo build`, `cargo test --release`, etc. The system matches by prefix — if the command starts with the permitted value, it's allowed.

- **Default read access to the current directory:** The agent can read any file inside the directory you launched tcode from — no permission prompt, no approval needed. This is granted automatically so the agent can explore the project without you having to approve every file read. Writing still requires explicit permission.

  > **Be mindful of where you launch tcode.** If you run `tcode` from `/home/user`, the agent can read everything under your home directory. Always launch tcode from the specific project directory you want to work in.

- **Bash commands and file paths — best-effort analysis, not a sandbox.** When the agent runs a bash command, the permission system parses it with tree-sitter-bash and walks the syntax tree to pull out file paths it can recognize: arguments to a small whitelist of read-only commands (`cat`, `head`, `tail`, `wc`, `stat`, …), arguments to constructive-write commands (`mkdir`, `touch`), and shell redirections (`<`, `>`, `>>`). Those extracted paths *are* matched against your `file_read`/`file_write` grants, so granting `file_read > path > /home/user/myproject` will silently cover `cat /home/user/myproject/README.md` without a new prompt.

  Beyond that whitelist, the permission system does not track file access. Everything else — `sed`, `python script.py`, `make`, a shell function, any binary on `$PATH` — is gated only by the `bash > command` prefix check. Once the prefix is approved (or a complex command is allowed once), the process runs with the full filesystem access of your OS user. **`file_read`/`file_write` path grants do not prevent a permitted bash command from reading or writing files outside those paths**, and the syntax analysis itself is best-effort: a sufficiently clever command line can defeat it. If you need real isolation, use OS-level tools (containers, VMs, dedicated user accounts).

- **Special case — complex commands:** Commands with pipes (`|`), chaining (`&&`, `||`, `;`), or other shell constructs are always prompted as "allow once" only. These can't be safely cached by prefix because the overall command may do something very different from what a simple prefix suggests.

## Approving permissions

When the agent needs to do something that requires permission, a pending request appears in the **permission pane** (bottom-right). You have two ways to approve:

1. **Ctrl-p from any pane** — opens the next pending request as a popup. Choose:
   - `1` — Allow once
   - `2` — Allow for session
   - `3` — Allow for project
   - `4` — Deny

2. **From the permission pane** — navigate to a pending request with `j`/`k`, press **Enter** to open the approval popup.

## Adding permissions proactively

You don't have to wait for the agent to ask. You can grant permissions in advance from the permission pane:

1. Navigate to a key node (e.g., `bash > command`) in the permission tree.
2. Press **Enter** or **o** to open the add-permission popup.
3. Type the value (e.g., `npm`).
4. Choose the scope: `2` for session, `3` for project.

This is useful when you know the agent will need certain permissions and you want to avoid repeated prompts.

## Revoking permissions

Navigate to any granted permission in the permission pane and press **Enter** to open the management popup. Press **r** to revoke it. This works for both session and project permissions — revoking a project permission also removes it from the on-disk file.

## The permission tree

The permission pane always shows the full skeleton of all tool categories and keys, even before any permissions have been requested. This gives you a clear overview of the entire permission space. Each value node shows its status:

- **Pending** — the agent is waiting for your decision
- **Session** — granted for this session only
- **Project** — granted persistently

Press **f** in the permission pane to toggle between showing all permissions and showing only pending ones.

## Tips

- **Start broad for trusted projects.** If you're working in your own project and trust the agent, granting `file_write > path` at the project root and a few command prefixes like `cargo` or `npm` as project permissions will cut down on prompts significantly.

- **Use "allow once" for unfamiliar commands.** If the agent wants to run something you haven't seen before, allow it once and see what happens. You can always grant broader permission later.

- **Revoke when done.** If you granted a broad permission temporarily (e.g., `bash > command > rm`), revoke it when you're done with that task.

- **Project permissions survive restarts.** If something feels wrong after restarting tcode, check the permission pane — old project permissions may still be active. Revoke anything you no longer need.
