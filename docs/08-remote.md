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
| `--allow-insecure-http` | Omit the `Secure` cookie attribute for direct plain-HTTP access. Use only for trusted local/private setups. |

Relevant global flags:

| Flag | Description |
|------|-------------|
| `-p <profile>` | Load `~/.tcode/config-<profile>.toml` instead of the default config. |
| `-c <container>` / `--container <container>` | In remote sessions, run bash commands inside an existing Docker/Podman container. File tools still operate on the host. See [Configuration: Container Mode](02-configuration.md#container-mode). |
| `--container-runtime <runtime>` | Container runtime CLI for `-c/--container`: `docker` (default) or `podman`. Requires `-c/--container`. |

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

For direct non-loopback plain HTTP, login may fail because the browser can discard `Secure` cookies. If you intentionally run on trusted plain HTTP, opt in explicitly:

```sh
tcode remote --host 0.0.0.0 --port 8080 --allow-insecure-http
```

This sends the password and session cookie over cleartext HTTP. Do not use it on untrusted networks.

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
- Project-local instructions such as `CLAUDE.md` are not loaded.
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
| Browser profile | `~/.tcode/chrome/` |

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

The browser profile is persistent:

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

The repository includes a Dockerfile for running the remote server in web-only mode. The image includes:

- `tcode`
- `browser-server`
- Chromium and Chromium sandbox support
- bundled web frontend
- a non-root `tcode` runtime user

The default container command is:

```sh
tcode remote --host 0.0.0.0 --port 8080
```

### Build the image

From the repository root:

```sh
./build-docker.sh
```

The script tags the image as:

- `tcode-remote:<git-short-hash>` when all changes are committed and the git tree is clean
- `tcode-remote:<year-month-day-unix-timestamp>` when there are uncommitted or untracked changes, for example `tcode-remote:2026-04-28-1777390212`

The script prints the exact tag after the build. The examples below use `"$IMAGE_TAG"` for that printed value:

```sh
IMAGE_TAG=tcode-remote:abc1234  # replace with the tag printed by ./build-docker.sh
```

### Run the image

For HTTPS/reverse-proxy use:

```sh
docker run --rm \
  --security-opt seccomp=unconfined \
  -p 8080:8080 \
  -v "$HOME/.tcode/web-users.toml:/home/tcode/.tcode/web-users.toml:ro" \
  "$IMAGE_TAG"
```

The `--security-opt seccomp=unconfined` option lets Chromium create the sandbox namespaces it needs inside Docker. Without it, Chromium may fail to launch and browser tools can hang until their timeout.

This starts the server, but model calls still need provider configuration. Mount a data directory containing `config.toml` and any OAuth tokens, or pass provider API-key environment variables such as `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or `OPENROUTER_API_KEY`.

For non-loopback direct plain-HTTP testing, add `--allow-insecure-http` by passing the full remote args after the image name. You should not need this for loopback access such as `http://127.0.0.1:8080/` when you use the same hostname consistently, or when accessing through HTTPS:

```sh
docker run --rm \
  --security-opt seccomp=unconfined \
  -p 8080:8080 \
  -v "$HOME/.tcode/web-users.toml:/home/tcode/.tcode/web-users.toml:ro" \
  "$IMAGE_TAG" \
  --host 0.0.0.0 --port 8080 --allow-insecure-http
```

### Persist container state

The image does not mount your host `~/.tcode` by default. Inside the container, tcode uses:

```text
/home/tcode/.tcode
```

For persistent state, mount a dedicated data directory:

```sh
mkdir -p "$HOME/tcode-docker-data"

docker run --rm \
  --security-opt seccomp=unconfined \
  -p 8080:8080 \
  -v "$HOME/tcode-docker-data:/home/tcode/.tcode:rw" \
  "$IMAGE_TAG"
```

Recommended practice is to copy only the data you want into this dedicated directory instead of mounting your real `~/.tcode`:

```sh
mkdir -p "$HOME/tcode-docker-data"
cp ~/.tcode/config.toml "$HOME/tcode-docker-data/"
cp ~/.tcode/web-users.toml "$HOME/tcode-docker-data/"
# Optional, only if needed:
# cp -a ~/.tcode/auth "$HOME/tcode-docker-data/"
# cp -a ~/.tcode/chrome "$HOME/tcode-docker-data/"
```

If your `web-users.toml` specifies a `trash_dir` outside the data directory (e.g. `/home/alice/.tcode/trash`), ensure that path is accessible from the container — either mount it explicitly or place it inside the mounted data directory so the server can write deleted sessions there.

Mounting your real `~/.tcode` is possible, but it exposes all local tcode config, auth, sessions, and browser data to the container:

```sh
docker run --rm \
  --security-opt seccomp=unconfined \
  -p 8080:8080 \
  -v "$HOME/.tcode:/home/tcode/.tcode:rw" \
  "$IMAGE_TAG"
```

### Docker browser profile data

The container uses:

```text
/home/tcode/.tcode/chrome
```

You can prepare browser data outside Docker with:

```sh
tcode browser
```

Then copy the profile into your Docker data directory:

```sh
cp -a ~/.tcode/chrome "$HOME/tcode-docker-data/"
```

The browser profile should be writable inside the container. Chromium writes locks, cookies, preferences, cache/state, and other runtime files.

### Docker image vs container mode

The Dockerfile runs tcode itself inside a container and serves the remote web UI.

This is different from tcode's `-c/--container` mode, where tcode runs on the host but bash commands execute inside an already-running Docker or Podman container. See [Configuration: Container Mode](02-configuration.md#container-mode).
