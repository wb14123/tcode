# Configuration Reference

## Config File Location

tcode stores configuration in TOML files under `~/.tcode/`:

- **Default config:** `~/.tcode/config.toml`
- **Profile configs:** `~/.tcode/config-<profile>.toml` — selected with `tcode -p <profile>`

Profiles are fully self-contained. They do not inherit from the default config. Any fields omitted from a config file fall back to built-in defaults.

Config files are created by the `tcode config` interactive wizard, which prompts for `provider`, `base_url`, and `api_key`. All other options (`model`, `subagent_*`, `browser_server_*`, `search_engine`, `[shortcuts]`, `[layout]`) are written as commented-out lines in the generated file — open the file in your editor and uncomment the ones you want to customize.

- **First launch.** If `~/.tcode/config.toml` does not exist and stdin/stdout are both TTYs, running `tcode` (with no profile) automatically launches the wizard, writes the file, prints the absolute path, and exits. Re-run `tcode` to start a session.
- **Explicit invocation.** Run `tcode config` any time to create the default config, or `tcode -p <profile> config` to write `~/.tcode/config-<profile>.toml`. Running `tcode config` against an existing file is a **hard error** — to regenerate, delete the file first and re-run the wizard.
- **File permissions.** On Unix the wizard writes the file with `0600` permissions via a temp-file + rename dance, so a crash or Ctrl-C mid-wizard does not leave a partial file at the real path.
- **Missing config file.** If the target file does not exist and tcode cannot auto-launch the wizard (a profile is specified, or a non-TTY context such as CI or a piped stdin), tcode exits with an error naming the absolute path and the exact command to run — `tcode config` for the default config, or `tcode -p <profile> config` for a profile.

See [03-commands.md](03-commands.md#tcode-config) for the full `tcode config` command reference.

## CLI Flags

```
tcode                          # start with default config
tcode -p work                  # start with ~/.tcode/config-work.toml
tcode --session <id> attach    # attach to existing session
```

The CLI only accepts `--session`, `-p`/`--profile`, and `-V`/`--version`. All other settings live in the config file.

## Full Config Reference

`provider` is required — there is no default. A config file missing the `provider` line causes tcode to exit with an error listing the valid values.

```toml
provider = "claude"              # REQUIRED. claude | claude-oauth | open-ai | open-router
                                 # "claude" is strictly API-key mode. "claude-oauth"
                                 # is its own provider — loads tokens from
                                 # `tcode claude-auth` and ignores api_key / env var.
api_key = ""                     # optional. Empty string and omitting the line behave
                                 # identically: both fall back to the provider env var
                                 # (see Providers table), then to "" (no auth) if the
                                 # env var is also unset. Ignored when
                                 # provider = "claude-oauth".
model = "claude-opus-4-6"        # defaults per provider
base_url = ""                    # defaults per provider
max_subagent_depth = 10
subagent_model_selection = false
browser_server_url = ""          # remote browser-server URL (TCP mode)
browser_server_token = ""        # bearer token for remote browser-server
search_engine = "google"         # kagi | google

[shortcuts]                      # see Shortcut Templates section below
brainstorm = "..."
```

## Providers

| Provider     | Config value    | Env Variable          | Default Model          | Default Base URL                |
|--------------|-----------------|-----------------------|------------------------|---------------------------------|
| Claude       | `claude`        | `ANTHROPIC_API_KEY`   | `claude-opus-4-6`      | `https://api.anthropic.com`     |
| Claude OAuth | `claude-oauth`  | *(none — OAuth-only)* | `claude-opus-4-6`      | `https://api.anthropic.com`     |
| OpenAI       | `open-ai`       | `OPENAI_API_KEY`      | `gpt-5-nano`           | `https://api.openai.com/v1`     |
| OpenRouter   | `open-router`   | `OPENROUTER_API_KEY`  | `deepseek/deepseek-r1` | `https://openrouter.ai/api/v1`  |

`claude-oauth` ignores both `api_key` in the config file and `$ANTHROPIC_API_KEY` in the environment — it authenticates exclusively via the tokens written by `tcode claude-auth`.

**API-key resolution.** For the three API-key providers (`claude`, `open-ai`, `open-router`), tcode resolves the credential in this order: (1) a non-empty `api_key` in the config file, (2) otherwise a non-empty value from the provider's environment variable, (3) otherwise the empty string, which is passed through to the HTTP client as-is. tcode no longer errors at startup on a missing API key — if neither source is set, the first LLM call inside the TUI fails with an HTTP 401/403 from the provider. This is convenient for self-hosted endpoints that do not require auth and a minor nuisance otherwise.

**Claude Pro/Max subscribers** can use their subscription credits via the API instead of paying for API usage separately. In the `tcode config` wizard, choose **`claude-oauth`** instead of **`claude`**. The wizard skips both the base URL and API-key prompts and writes `provider = "claude-oauth"` to the config file — this is a distinct provider value, not a wizard-level label. After the wizard exits, run `tcode claude-auth` to complete OAuth. At runtime the `claude-oauth` provider loads tokens from `tcode claude-auth` and ignores `api_key` and `$ANTHROPIC_API_KEY` entirely, so there is no need to unset the env var.

**Migration note.** Earlier versions of tcode treated `provider = "claude"` with no `api_key` and no `$ANTHROPIC_API_KEY` as an implicit fallback to OAuth. That fallback is gone: `provider = "claude"` is now strictly API-key mode. If you were relying on the implicit fallback, edit your config and change the line to `provider = "claude-oauth"`.

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

Shortcuts let you expand short names into full prompts in the edit buffer. They are defined in the `[shortcuts]` section of your config file.

**Usage:**

1. Type `/shortcutname` at the start of a line or after a space.
2. `/` in those positions auto-shows a completion popup.
3. Typing narrows the popup to matching shortcuts.
4. Press Tab on an exact match to expand it, or press Enter on a popup selection to expand.

tcode ships with 5 built-in shortcuts (`brainstorm`, `plan`, `save-plan`, `implement-plan`, `review`). If no `[shortcuts]` section exists in your config, the defaults are used. To customize, add a `[shortcuts]` section:

```toml
[shortcuts]
brainstorm = "This is a brainstorm. Do not implement anything."
plan = """\
  Design and plan first. Do not implement or change any code before I confirm. \
  Ask me questions if there is anything not clear."""
my-shortcut = "Custom shortcut text here."
```

Use TOML multi-line strings (`"""\...\"""`) with trailing backslashes for long templates. Setting `[shortcuts]` to an empty section disables all shortcuts.

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
