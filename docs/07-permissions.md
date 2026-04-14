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

- **`*` (wildcard)** is a reserved magic value that matches any value under its (tool, key) pair. See [Wildcards](#wildcards) below.

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

- **Compound commands — decomposed, not blanket-approved.** Commands with pipes (`|`) or sequential chaining (`&&`, `||`, `;`) are parsed and split into their individual sub-commands, and each sub-command is checked against your permissions independently. So `mkdir foo && ls foo` produces a `file_write` check on `foo` (from `mkdir`) and a separate `bash > command > ls` check — each can be allowed once, for the session, or for the project, and cached grants apply per sub-command. Any top-level redirection (`>`, `>>`, `<`) also enforces the matching `file_write`/`file_read` path check on the redirect target.

- **Non-decomposable commands — allow once only.** Commands that use constructs the parser cannot safely split — command substitution (`` `...` ``, `$(...)`), subshells (`(...)`), process substitution (`<(...)`), variable expansion into commands, and `eval` — are always prompted as "allow once" only. These can't be safely cached because the actual commands executed depend on runtime expansion.

## Approving permissions

When the agent needs to do something that requires permission, a pending request appears in the **permission pane** (bottom-right). You have two ways to approve:

1. **Ctrl-p from any pane** — opens the next pending request as a popup. Choose:
   - `1` — Allow once
   - `2` — Allow for session
   - `3` — Allow for project
   - `4` — Deny (with optional reason)

2. **From the permission pane** — navigate to a pending request with `j`/`k`, press **Enter** to open the approval popup.

### Denying with a reason

Pressing `4` in the approval popup does not immediately resolve the request — it opens a small single-line text input where you can type a short reason. Press **Enter** to deny (empty input denies without a reason; leading/trailing whitespace is stripped, so a space-only input is treated the same as empty). Press **Esc** to go back to the approval menu, or **Ctrl-C** to cancel the popup entirely.

The reason, if given, is appended to the permission-denied error itself, so the same text appears verbatim both in the tool output you see in the main display and in what the agent receives — a single source of truth. For example, denying a `grep` call with the reason `"use rg instead"` lets the agent switch to `rg` on its own without re-prompting you. (Embedded newlines or tabs in the reason are collapsed to single spaces, so a multi-line reason can never break the single-line display layout.) If you deny without a reason, the agent is told only that the request was declined and asked to check in with you.

## Adding permissions proactively

You don't have to wait for the agent to ask. You can grant permissions in advance from the permission pane:

1. Navigate to a key node (e.g., `bash > command`) in the permission tree.
2. Press **Enter** or **o** to open the add-permission popup.
3. In the menu, choose:
   - `1` — **Enter a specific value**, then type it in (e.g., `npm`).
   - `2` — **Allow all values (`*`)** — grants a wildcard permission for every value under this key. See [Wildcards](#wildcards).
4. Choose the scope: `2` for session, `3` for project.

This is useful when you know the agent will need certain permissions and you want to avoid repeated prompts. Note that `*` is a reserved magic value: you cannot type it into the specific-value input — use option `[2]` instead.

## Wildcards

You can grant a **wildcard** permission with the reserved value `*`. A wildcard matches any value under its (tool, key) pair:

| Wildcard | Effect |
|----------|--------|
| `file_read > path > *` | Read any file anywhere on the filesystem |
| `file_write > path > *` | Write to any file anywhere on the filesystem |
| `bash > command > *` | Run any bash command |
| `web_fetch > hostname > *` | Fetch any hostname |

Wildcards are added from the add-permission popup — on a key node, press **Enter**/**o** and choose `[2] Allow all values (*)`. In the tree view, wildcard leaves are sorted first under their key and rendered as `[S] * (allow all) (session)`.

Wildcards are useful for short, trusted sessions where you don't want to be prompted at all. Prefer **session** scope so they disappear when you close tcode.

### Wildcard safety for bash

`bash > command > *` is deliberately scoped so that **file defenses are not bypassed**. Even with the wildcard granted:

- Top-level redirections (`cmd > file`, `cmd < file`, `cmd >> file`) still require the matching `file_read`/`file_write` path permission for the redirect target.
- Commands the parser classifies as read-only (`cat`, `head`, `tail`, `wc`, `stat`, …) are routed through `file_read > path` on their arguments, not the bash wildcard.
- Commands the parser classifies as constructive writes (`mkdir`, `touch`) are routed through `file_write > path` on their arguments.
- Compound commands (`cmd1 && cmd2`, `a | b`, `x; y`) are decomposed and each sub-command is checked independently. The bash wildcard only short-circuits each individual sub-command that reaches the final "other simple command" classification.

The one place `bash > command > *` gives a true blanket pass is **non-decomposable complex commands** (eval, command substitution, subshells, process substitution) — there the parser cannot see inside, so the wildcard is the documented escape hatch. If you don't want that escape hatch, don't grant the bash wildcard.

## Revoking permissions

Navigate to any granted permission in the permission pane and press **Enter** to open the management popup. Press **r** to revoke it. This works for both session and project permissions — revoking a project permission also removes it from the on-disk file.

## The permission tree

The permission pane always shows the full skeleton of all tool categories and keys, even before any permissions have been requested. This gives you a clear overview of the entire permission space. Each value node shows its status:

- **Pending** — the agent is waiting for your decision
- **Session** — granted for this session only
- **Project** — granted persistently

Press **f** in the permission pane to toggle between showing all permissions and showing only pending ones.

### Container mode annotations

When running with `-c` (container mode), the permission pane annotates each tool to show where it executes. The `bash` tool is labeled `bash (in container X)` while all other tools (file_read, file_write, web_fetch) are labeled with `(outside container)`. This makes it clear which operations run inside the container sandbox and which operate directly on the host.

## Tips

- **Start broad for trusted projects.** If you're working in your own project and trust the agent, granting `file_write > path` at the project root and a few command prefixes like `cargo` or `npm` as project permissions will cut down on prompts significantly.

- **Use "allow once" for unfamiliar commands.** If the agent wants to run something you haven't seen before, allow it once and see what happens. You can always grant broader permission later.

- **Revoke when done.** If you granted a broad permission temporarily (e.g., `bash > command > rm`), revoke it when you're done with that task.

- **Project permissions survive restarts.** If something feels wrong after restarting tcode, check the permission pane — old project permissions may still be active. Revoke anything you no longer need.
