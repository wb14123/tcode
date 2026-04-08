# Configuration Reference

## Config File Location

tcode stores configuration in TOML files under `~/.tcode/`:

- **Default config:** `~/.tcode/config.toml`
- **Profile configs:** `~/.tcode/config-<profile>.toml` — selected with `tcode -p <profile>`

Profiles are fully self-contained. They do not inherit from the default config. If a profile file is missing, tcode exits with an error. Any fields omitted from a config file fall back to built-in defaults.

On first run, tcode auto-creates `~/.tcode/config.toml` with all options commented out as a template. The file is created with `0600` permissions to protect API keys.

## CLI Flags

```
tcode                          # start with default config
tcode -p work                  # start with ~/.tcode/config-work.toml
tcode --session <id> attach    # attach to existing session
```

The CLI only accepts `--session` and `-p`/`--profile`. All other settings live in the config file.

## Full Config Reference

```toml
provider = "claude"              # claude | open-ai | open-router
api_key = ""                     # or set env var (see Providers table)
model = "claude-opus-4-6"        # defaults per provider
base_url = ""                    # defaults per provider
subagent_max_iterations = 50
max_subagent_depth = 10
subagent_model_selection = false
browser_server_url = ""          # remote browser-server URL (TCP mode)
browser_server_token = ""        # bearer token for remote browser-server
search_engine = "kagi"           # kagi | google
```

## Providers

| Provider   | Config value  | Env Variable         | Default Model            | Default Base URL                |
|------------|---------------|----------------------|--------------------------|---------------------------------|
| Claude     | `claude`      | `ANTHROPIC_API_KEY`  | `claude-opus-4-6`        | `https://api.anthropic.com`     |
| OpenAI     | `open-ai`     | `OPENAI_API_KEY`     | `gpt-5-nano`             | `https://api.openai.com/v1`     |
| OpenRouter | `open-router` | `OPENROUTER_API_KEY` | `deepseek/deepseek-r1`   | `https://openrouter.ai/api/v1`  |

If `api_key` is not set in the config file, tcode reads the provider's environment variable instead.

**Claude Pro/Max subscribers** can use their subscription credits via the API instead of paying for API usage separately. Run `tcode claude-auth` to authenticate.

## Layout Configuration

The layout defines how tmux panes are arranged using a binary split tree. Each node is either a **split** (with children `a` and `b`) or a **command** (a leaf pane).

- **Split directions:** `horizontal` (left/right) or `vertical` (top/bottom).
- **Sizes** are percentages. Sibling sizes must add up to 100. The root node has no `size`.
- **Available commands:** `display`, `edit`, `tree`, `permission`.
- The layout must contain exactly one `display` pane, at least one `edit` pane, and at most one pane with `focus = true`.

Default layout:

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

This produces a two-column layout: the left column (70%) holds the display pane above and edit pane below; the right column (30%) holds the tree pane above and permission pane below.

## Browser Server

By default, tcode auto-manages a local browser-server process communicating via Unix socket at `~/.tcode/browser-server.sock`. Multiple tcode sessions share the same server instance, and it exits automatically after 5 minutes of inactivity.

For a remote browser-server, set both fields in your config:

```toml
browser_server_url = "http://remote-host:9222"
browser_server_token = "your-bearer-token"
```

See [06-browser.md](06-browser.md) for browser setup details.

## Shortcut Templates

Shortcuts let you expand short names into full prompts in the edit buffer.

**File:** `~/.tcode/shortcuts.lua`

**Setup:** Copy from `tcode/config/shortcuts.lua`, or let `install.sh` copy it automatically.

**Usage:**

1. Type `/shortcutname` at the start of a line or after a space.
2. `/` in those positions auto-shows a completion popup.
3. Typing narrows the popup to matching shortcuts.
4. Press Tab on an exact match to expand it, or press Enter on a popup selection to expand.

Example `shortcuts.lua`:

```lua
return {
  review = [[Review this code for correctness and edge cases.]],
  explain = "Explain this code in detail.",
  ["my-shortcut"] = [[Use ["quoted-key"] syntax for names with hyphens.]],
}
```

## CLAUDE.md

Place a `CLAUDE.md` file in your project root to inject custom instructions into every conversation. Its content is appended to the system prompt automatically.

**Location:** `<project-root>/CLAUDE.md`

**Behavior:**

- Loaded every time the system prompt is built (both root agent and subagents).
- The entire file content is appended to the end of the system prompt.
- If the file doesn't exist, it is silently skipped.
- No size limit is enforced — keep it concise for best results.

Example `CLAUDE.md`:

```markdown
# Project Notes

- This project uses parking_lot instead of std::sync.
- Always run `cargo fmt` after making changes.
- Prefer returning Result over unwrap in production code.
```

## Skills

Skills are reusable instruction sets the agent can load on demand via a tool call. Unlike CLAUDE.md (which is always present in the system prompt), skills are only loaded when the agent decides they are relevant.

**Directory structure:**

```
<skill-dir>/
  my-skill/
    SKILL.md
    helper.sh        # optional companion files
  another-skill/
    SKILL.md
```

**Scan locations (first match wins):**

1. `<project-root>/.tcode/skills/`
2. `<project-root>/.claude/skills/`
3. `~/.tcode/skills/`
4. `~/.claude/skills/`

If the same skill name appears in multiple directories, the first one found is used and duplicates produce a warning.

**SKILL.md format:** Markdown with optional YAML frontmatter:

```markdown
---
name: my-skill
description: One-line summary shown in skill listings
when_to_use: Guidance for the agent on when to invoke this skill
---

Detailed instructions go here.

Use ${CLAUDE_SKILL_DIR} to reference the skill's own directory.
```

**Key details:**

- **Name:** Defaults to the directory name if not set in frontmatter. Capped at 100 characters.
- **Companion files:** Each skill directory can include up to 10 additional files (non-recursive). These are listed alongside the skill content when loaded.
- **`${CLAUDE_SKILL_DIR}`:** Replaced with the absolute path to the skill directory at load time, useful for referencing companion scripts or data files.
- **Registration:** Skills are scanned once at startup. The `skill` tool is only registered if at least one skill is found.
