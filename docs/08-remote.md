# Remote Web UI

`tcode remote` starts the browser-based tcode UI and backend. It serves both the single-page web app and `/api/...` routes from the same origin.

Use it when you want to access tcode from a browser instead of the tmux/neovim UI, or when you want a remote/web-only research server.

## Quick start on localhost

Start the server with a shared login secret:

```sh
TCODE_REMOTE_PASSWORD='choose-a-strong-password' tcode remote --port 8080
```

Open:

```text
http://127.0.0.1:8080/
```

Log in with the value of `TCODE_REMOTE_PASSWORD`.

The default bind address is `127.0.0.1`, so this command is intended for same-machine browser access or use through a trusted tunnel.

## Command reference

```sh
tcode [global flags] remote --port <port> [options]
```

Common examples:

```sh
# Localhost only
TCODE_REMOTE_PASSWORD='...' tcode remote --port 8080

# Use a config profile
TCODE_REMOTE_PASSWORD='...' tcode -p work remote --port 8080

# Web-only remote server
TCODE_REMOTE_PASSWORD='...' tcode --web-only remote --port 8080

# Bind on all interfaces, for use behind a reverse proxy or trusted network boundary
TCODE_REMOTE_PASSWORD='...' tcode remote --host 0.0.0.0 --port 8080
```

Remote-specific flags:

| Flag | Description |
|------|-------------|
| `--port <port>` | TCP port to bind. Required. `0` is rejected; choose a concrete port. |
| `--host <ip>` | IP address to bind. Defaults to `127.0.0.1`. Use `0.0.0.0` or `::` only when intentionally exposing the server beyond localhost. |
| `--password <secret>` | Shared secret for browser login. Prefer `TCODE_REMOTE_PASSWORD` instead. |
| `--allow-insecure-http` | Omit the `Secure` cookie attribute for direct plain-HTTP access. Use only for trusted local/private setups. |

Relevant global flags:

| Flag | Description |
|------|-------------|
| `-p <profile>` | Load `~/.tcode/config-<profile>.toml` instead of the default config. |
| `--web-only` | Create and expose only web-only sessions from this remote server. |
| `-c <container>` / `--container <container>` | In normal remote sessions, run bash commands inside an existing Docker/Podman container. File tools still operate on the host. See [Configuration: Container Mode](02-configuration.md#container-mode). |

## Password and login sessions

`tcode remote` uses a single shared secret, not per-user accounts.

Prefer the environment variable:

```sh
TCODE_REMOTE_PASSWORD='choose-a-strong-password' tcode remote --port 8080
```

Passing the secret on argv works, but is less safe because command arguments can appear in shell history or process listings:

```sh
tcode remote --port 8080 --password 'choose-a-strong-password'
```

Notes:

- Empty or all-whitespace passwords are rejected.
- Passwords shorter than 16 characters produce a warning.
- Leading/trailing whitespace is significant at login.
- Login creates an HTTP-only `tcode_session` cookie with a 7-day max age.
- Server-side login sessions are in memory. Restarting `tcode remote` invalidates existing browser logins.

## Security model

`tcode remote` does not provide built-in TLS. It starts a plain HTTP server.

Safe defaults:

- Binds to `127.0.0.1` unless you pass `--host`.
- Uses `Secure`, `HttpOnly`, `SameSite=Strict` auth cookies by default.
- Applies same-origin checks to write routes.
- Serves the web app with restrictive browser security headers, including a CSP and `frame-ancestors 'none'`.

When exposing beyond localhost:

1. Use a strong password.
2. Prefer HTTPS through a reverse proxy, SSH tunnel, or other trusted tunnel/proxy.
3. Bind intentionally, for example `--host 0.0.0.0`, only when the network path is protected.
4. Consider `--web-only` unless you specifically need normal sessions with local file/shell tools.

For HTTPS reverse proxies, preserve the original `Host` header and set either `X-Forwarded-Proto: https` or `Forwarded: proto=https`. tcode uses these headers when checking request origins for login/logout and other write routes. Proxy paths unchanged so `/api/...` and browser routes stay on the same origin.

### Plain HTTP access

For default loopback access such as `http://127.0.0.1:8080/`, browsers generally treat loopback origins as secure enough for local development behavior.

For direct non-loopback plain HTTP, login may fail because the browser can discard `Secure` cookies. If you intentionally run on trusted plain HTTP, opt in explicitly:

```sh
TCODE_REMOTE_PASSWORD='...' \
  tcode remote --host 0.0.0.0 --port 8080 --allow-insecure-http
```

This sends the password and session cookie over cleartext HTTP. Do not use it on untrusted networks.

## Web-only remote mode

For a safer research-oriented remote server, run:

```sh
TCODE_REMOTE_PASSWORD='...' tcode --web-only remote --port 8080
```

or, inside Docker, the image defaults to the equivalent of:

```sh
tcode remote --web-only --host 0.0.0.0 --port 8080
```

In web-only mode:

- New sessions are created as web-only sessions.
- Existing non-web-only sessions are hidden from this remote server.
- Project-local instructions such as `CLAUDE.md` are not loaded.
- No current working directory is captured.
- Local filesystem, shell, edit, grep/glob, LSP, and skill tools are not registered.
- Available tools are limited to:
  - `current_time`
  - `web_search`
  - `web_fetch`
  - `subagent`
  - `continue_subagent`

This makes `--web-only` a better default for remotely exposed browser access. Normal remote sessions expose the same local/container capabilities as normal tcode sessions and should be treated as high trust.

## Configuration and data paths

`tcode remote` uses the same config and data locations as the terminal UI:

| Data | Location |
|------|----------|
| Default config | `~/.tcode/config.toml` |
| Profile config | `~/.tcode/config-<profile>.toml` |
| OAuth tokens | `~/.tcode/auth/` |
| Sessions | `~/.tcode/sessions/` |
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
tcode remote --web-only --host 0.0.0.0 --port 8080
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
  -e TCODE_REMOTE_PASSWORD='choose-a-strong-password' \
  "$IMAGE_TAG"
```

The `--security-opt seccomp=unconfined` option lets Chromium create the sandbox namespaces it needs inside Docker. Without it, Chromium may fail to launch and browser tools can hang until their timeout.

This starts the server, but model calls still need provider configuration. Mount a data directory containing `config.toml` and any OAuth tokens, or pass provider API-key environment variables such as `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or `OPENROUTER_API_KEY`.

For non-loopback direct plain-HTTP testing, add `--allow-insecure-http` by passing the full remote args after the image name. You should not need this for `http://127.0.0.1:8080/`, `http://localhost:8080/`, or when accessing through HTTPS:

```sh
docker run --rm \
  --security-opt seccomp=unconfined \
  -p 8080:8080 \
  -e TCODE_REMOTE_PASSWORD='choose-a-strong-password' \
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
  -e TCODE_REMOTE_PASSWORD='choose-a-strong-password' \
  -v "$HOME/tcode-docker-data:/home/tcode/.tcode:rw" \
  "$IMAGE_TAG"
```

Recommended practice is to copy only the data you want into this dedicated directory instead of mounting your real `~/.tcode`:

```sh
mkdir -p "$HOME/tcode-docker-data"
cp ~/.tcode/config.toml "$HOME/tcode-docker-data/"
# Optional, only if needed:
# cp -a ~/.tcode/auth "$HOME/tcode-docker-data/"
# cp -a ~/.tcode/chrome "$HOME/tcode-docker-data/"
```

Mounting your real `~/.tcode` is possible, but it exposes all local tcode config, auth, sessions, and browser data to the container:

```sh
docker run --rm \
  --security-opt seccomp=unconfined \
  -p 8080:8080 \
  -e TCODE_REMOTE_PASSWORD='choose-a-strong-password' \
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
