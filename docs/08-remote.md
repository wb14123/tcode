# Remote Web UI

`tcode remote` starts the browser-based tcode UI and backend. It serves both the single-page web app and `/api/...` routes from the same origin.

Use it when you want to access tcode from a browser instead of the tmux/neovim UI, or when you want a remote/web-only research server.

## Quick start on localhost

First, create a web user:

```sh
tcode add-web-user alice
```

You'll be prompted for a password and a trash directory. The user, a hashed password, and the trash path are stored in `~/.tcode/web-users.toml`.

Then start the server:

```sh
tcode remote --port 8080
```

Open:

```text
http://127.0.0.1:8080/
```

Log in with the username and password you chose.

The default bind address is `127.0.0.1`, so this command is intended for same-machine browser access or use through a trusted tunnel. The examples use `127.0.0.1`; if you choose another loopback hostname such as `localhost`, use that hostname consistently in the browser because cookies and same-origin checks are origin-specific.

The server requires `~/.tcode/web-users.toml` to exist with at least one user. If the file is missing or empty, the server exits with an error.

## Command reference

```sh
tcode [global flags] remote --port <port> [options]
```

Common examples:

```sh
# Localhost only
tcode remote --port 8080

# Use a config profile
tcode -p work remote --port 8080

# Bind on all interfaces, for use behind a reverse proxy or trusted network boundary
tcode remote --host 0.0.0.0 --port 8080
```

Remote-specific flags:

| Flag | Description |
|------|-------------|
| `--port <port>` | TCP port to bind. Required. `0` is rejected; choose a concrete port. |
| `--host <ip>` | IP address to bind. Defaults to `127.0.0.1`. Use `0.0.0.0` or `::` only when intentionally exposing the server beyond localhost. |
| `--allow-insecure-http` | Omit the `Secure` cookie attribute for plain HTTP. Opt in only for local loopback development; use HTTPS for exposed deployments. |

Relevant global flags:

| Flag | Description |
|------|-------------|
| `-p <profile>` | Load `~/.tcode/config-<profile>.toml` instead of the default config. |
| `-c <container>` / `--container <container>` | In remote sessions, run bash commands inside an existing Docker/Podman container. File tools still operate on the host. See [Configuration: Container Mode](02-configuration.md#container-mode). |
| `--container-runtime <runtime>` | Container runtime CLI for `-c/--container`: `docker` (default) or `podman`. Requires `-c/--container`. |
| `--no-container` | Force no container, even if `-c/--container` or project config specifies one. Conflicts with `-c/--container`. |

`--session <id>` is a global flag for terminal/tmux session commands. It is not used by `tcode remote`: the web server lists and creates sessions through the browser UI instead of attaching to one startup session.

## Users and login sessions

`tcode remote` uses per-user accounts with argon2id-hashed passwords stored in `~/.tcode/web-users.toml`.

Create users before starting the server:

```sh
tcode add-web-user alice
```

You'll be prompted for a password interactively. The password is never echoed and never stored in shell history. Run the command again to add more users:

```sh
tcode add-web-user bob
```

The resulting file looks like this:

```toml
# ~/.tcode/web-users.toml
[users.alice]
password_hash = "$argon2id$v=19$m=65536,t=3,p=4$..."
session_dir = "/home/alice/.tcode/sessions"
trash_dir = "/home/alice/.tcode/trash"

[users.bob]
password_hash = "$argon2id$v=19$m=65536,t=3,p=4$..."
session_dir = "/home/bob/.tcode/sessions"
trash_dir = "/home/bob/.tcode/trash"
```

Notes:

- Passwords shorter than 8 characters are rejected by `tcode add-web-user`.
- Login sends `POST /api/auth/login` with `{"username": "...", "password": "..."}`.
- Successful login returns an HTTP-only `tcode_session` cookie with a 7-day max age.
- Server-side login sessions are in memory. Restarting `tcode remote` invalidates existing browser logins.
- Each user has a `trash_dir` for deleted sessions. The server validates it exists and is writable at startup.

## Security model

`tcode remote` does not provide built-in TLS. It starts a plain HTTP server.

Safe defaults:

- Binds to `127.0.0.1` unless you pass `--host`.
- Uses `Secure`, `HttpOnly`, `SameSite=Strict` auth cookies by default.
- Applies same-origin checks to write routes.
- Serves the web app with restrictive browser security headers, including a CSP and `frame-ancestors 'none'`.

When exposing beyond localhost:

1. Use strong passwords for all web users.
2. Prefer HTTPS through a reverse proxy, SSH tunnel, or other trusted tunnel/proxy.
3. Bind intentionally, for example `--host 0.0.0.0`, only when the network path is protected.

For HTTPS reverse proxies, preserve the original `Host` header and set either `X-Forwarded-Proto: https` or `Forwarded: proto=https`. tcode uses these headers when checking request origins for login/logout and other write routes. Proxy paths unchanged so `/api/...` and browser routes stay on the same origin.

### Plain HTTP access

For default loopback access such as `http://127.0.0.1:8080/`, browsers generally treat loopback origins as secure enough for local development behavior.

If your browser does not round-trip `Secure` cookies on loopback HTTP during
local development, explicitly opt in while keeping the loopback bind:

```sh
tcode remote --host 127.0.0.1 --port 8080 --allow-insecure-http
```

This sends the password and session cookie over cleartext HTTP; it does not
add TLS. Keep this override local and use HTTPS for exposed deployments.

## Web UI capabilities

After login, the browser UI can:

- list existing sessions and show each session's status and mode (`normal` or `web-only`)
- create a new conversation, optionally with an initial prompt
- stream the live conversation timeline and token/usage/status updates
- send follow-up messages to root conversations and subagents
- open subagent views and tool-call detail views
- cancel active conversations, subagents, and tool calls
- mark a subagent as done when it has completed its reply
- review pending permission requests and allow or deny them from the permission modal
- move sessions to trash from the Manage Conversations page

Normal remote sessions have the same local/container capabilities as terminal tcode sessions, so permission prompts and grants have the same security meaning. The current browser UI focuses on resolving pending permission requests; the backend API also exposes permission add/revoke endpoints for clients and integrations.

The backend API is documented in [`tcode-web/api.md`](../tcode-web/api.md). It includes auth endpoints, session lifecycle endpoints, file-shaped read/stream endpoints, subagent and tool-call endpoints, conversation/tool cancellation, and permission APIs.

## Web session mode

All sessions created through the web UI are web-only sessions — no flag is needed. The browser remote server does not create or expose normal (filesystem/shell) sessions.

In web-only mode:

- New sessions are created as web-only sessions.
- Project-local instructions such as `CLAUDE.md` or `AGENTS.md` are not loaded.
- No current working directory is captured.
- Local filesystem, shell, edit, grep/glob, LSP, and skill tools are not registered.
- Available tools are limited to:
  - `current_time`
  - `web_search`
  - `web_fetch`
  - `subagent`
  - `continue_subagent`
- `web_fetch` hostname permissions are auto-granted (a session-scoped wildcard `web_fetch > hostname > *`). The grant appears in the permission tree and can be revoked at any time.

This makes the web remote server safe for remotely exposed browser access. Normal sessions with local/container capabilities are available only through the terminal/tmux UI.

## Configuration and data paths

`tcode remote` uses the same config and data locations as the terminal UI:

| Data | Location |
|------|----------|
| Default config | `~/.tcode/config.toml` |
| Profile config | `~/.tcode/config-<profile>.toml` |
| Web users | `~/.tcode/web-users.toml` |
| OAuth tokens | `~/.tcode/auth/` |
| Sessions | `~/.tcode/sessions/` |
| Trash | `~/.tcode/trash/` (per-user, configured in `web-users.toml`) |
| Browser-server socket | `~/.tcode/browser-server.sock` |
| Browser profile | `~/.tcode/chrome/` (or `~/snap/<name>/common/.tcode-chrome/` for snap packages) |

Provider API keys can be stored in the config file or passed via environment variables:

| Provider | Environment variable |
|----------|----------------------|
| Claude API-key mode | `ANTHROPIC_API_KEY` |
| OpenAI API-key mode | `OPENAI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |

OAuth providers use token files under `~/.tcode/auth/`; use the same profile for auth and runtime.

## Browser tools

`web_search` and `web_fetch` use `browser-server` and Chromium/Chrome.

By default, tcode auto-starts a sibling `browser-server` binary and talks to it through:

```text
~/.tcode/browser-server.sock
```

The browser profile is persistent (see
[Profile location](06-browser.md#profile-location) for the snap-aware
path):

```text
~/.tcode/chrome/
```

Use this command outside remote mode to open the persistent profile interactively and log in to services such as Google or Kagi:

```sh
tcode browser
```

Do not use the same browser profile concurrently from multiple browser-server or `tcode browser` processes. Chrome profile locks can prevent startup until the other process exits.

You can also configure a remote browser-server with `browser_server_url` and `browser_server_token` in `config.toml`; see [Configuration: Browser Server](02-configuration.md#browser-server).

## Frontend serving

Installed release binaries and `install-from-source.sh` builds embed the web frontend into the `tcode` binary.

Development builds without `--features tcode/bundled-frontend` serve files from:

```text
tcode-web/frontend/dist
```

If that directory is missing, API routes can still work, but browser UI routes return `404`. Build the frontend first:

```sh
cd tcode-web/frontend
npm ci
npm run build
```

## Docker web-only deployment

These commands use rootless Podman on Linux without a `--userns` override.
If `~/tcode-web-vol` already contains a browser profile, valid configuration,
and web users with persistent paths, skip setup and go to step 4.

### 1. Build the image

Run this command from the repository root:

```sh
./build-docker.sh podman
```

### 2. Copy the browser profile

Follow [Browser Setup](06-browser.md) to prepare and locate your host browser
profile, then stop every browser or tcode process using it. Copy the complete
profile directory, not just `Default`, into a new, dedicated state directory.
Replace the source below if your profile is elsewhere, and continue only if
both commands succeed:

```sh
mkdir -m 700 ~/tcode-web-vol && cp -a ~/.tcode/chrome ~/tcode-web-vol/chrome
```

Podman's `:U` recursively changes ownership of this host directory and may
prevent your host user from accessing it. Never mount your original profile,
live `~/.tcode`, or files linked to them. The copied profile must be exclusive
to one container or browser at a time, including during setup.

### 3. Configure tcode and create a web user

Both configuration and a web user are required. Run these commands one at a
time before starting the server:

```sh
podman run --rm -it --init -v ~/tcode-web-vol:/home/tcode/.tcode:rw,U localhost/tcode-remote:latest config
podman run --rm -it --init -v ~/tcode-web-vol:/home/tcode/.tcode:rw,U localhost/tcode-remote:latest add-web-user alice
```

When prompted, enter `/home/tcode/.tcode/sessions/alice` for the session
directory and `/home/tcode/.tcode/trash/alice` for the trash directory so both
persist in the host volume. See [Configuration](02-configuration.md#providers)
and the [CLI reference](03-commands.md) for provider and authentication details.

### 4. Run the server

```sh
podman run --rm --init --name tcode-remote -p 127.0.0.1:8080:8080 -v ~/tcode-web-vol:/home/tcode/.tcode:rw,U localhost/tcode-remote:latest
```

Open `http://127.0.0.1:8080/` and log in. The port is published only on host
loopback. If local HTTP login requires insecure cookies, append
`remote --host 0.0.0.0 --port 8080 --allow-insecure-http` after the image name;
keep the loopback port mapping.

To update an existing container, rebuild the image and recreate the container;
the `latest` tag does not update a running container automatically.

### Docker alternative

For conventional Linux Docker without user-namespace remapping, build as your
non-root host user:

```sh
./build-docker.sh docker \
  --build-arg TCODE_UID="$(id -u)" --build-arg TCODE_GID="$(id -g)"
```

Prepare a fresh, host-owned `~/tcode-web-vol-docker` using the same copying
steps; do not reuse the Podman-owned copy. Adapt both setup commands above to
use `docker`, the image `tcode-remote:latest`, and the mount
`-v ~/tcode-web-vol-docker:/home/tcode/.tcode:rw` without Podman's `:U` option.
Keep the same persistent session and trash paths, then start the server:

```sh
docker run --rm --init --name tcode-remote -p 127.0.0.1:8080:8080 -v ~/tcode-web-vol-docker:/home/tcode/.tcode:rw tcode-remote:latest
```
