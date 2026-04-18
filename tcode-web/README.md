# tcode-web

`tcode-web` is the web UI/backend for `tcode`.

For the current PoC:
- the frontend is built with Lit + Vite + TypeScript
- the Rust server serves the built frontend from the same origin
- auth uses the existing cookie-based login flow

## Build

### 1. Build the frontend

From the repo root:

```bash
cd tcode-web/frontend
npm install
npm run build
```

This writes the production bundle to:

```text
tcode-web/frontend/dist
```

The Rust server will serve those files if that directory exists.

### 2. Build/check the Rust crate

From the repo root:

```bash
cargo check -p tcode-web
```

Optional full CLI build:

```bash
cargo build -p tcode
```

## Run

Start the remote web server from the repo root.

Preferred form, using an env var for the shared secret:

```bash
TCODE_REMOTE_PASSWORD=change-me cargo run -p tcode -- remote --port 8080
```

If you want to select a profile:

```bash
TCODE_REMOTE_PASSWORD=change-me cargo run -p tcode -- -p <profile> remote --port 8080
```

You can also pass the password on argv, but the env var is preferred so it does not leak into shell history / `ps` output:

```bash
cargo run -p tcode -- remote --port 8080 --password change-me
```

## Open the app

Open:

```text
http://127.0.0.1:8080/
```

Use `127.0.0.1`, not `localhost`.

Then log in with the shared secret you passed at startup.

## Development notes

- If `tcode-web/frontend/dist` is missing, the Rust server will still run, but frontend routes will return `404`.
- After frontend changes, rebuild with `npm run build` before restarting or refreshing the Rust-served app.
- API routes stay under `/api/...`; non-API browser routes use SPA fallback.

## Color theme structure

The frontend theme lives in:

```text
tcode-web/frontend/src/styles.css
```

The current light theme uses a muted palette inspired by traditional Chinese colors, with a jade-teal primary accent for interactive elements.

### Theme token groups

The root `:root` block defines the main CSS variables used across the app:

- **Background and surfaces**
  - `--bg`, `--bg-top`, `--bg-bottom`
  - `--panel`, `--panel-soft`, `--panel-strong`
  - `--sidebar-bg`, `--input-bg`, `--pre-bg`
- **Typography and structure**
  - `--text`, `--text-muted`, `--text-subtle`
  - `--border`, `--border-strong`
  - `--shadow`, `--overlay`
- **Interactive accent**
  - `--accent`, `--accent-strong`, `--accent-soft`
  - `--focus-ring`, `--focus-border`
- **Semantic colors**
  - `--success`, `--success-soft`, `--success-border`, `--success-text`
  - `--warning`, `--warning-soft`, `--warning-border`, `--warning-text`
  - `--danger`, `--danger-soft`, `--danger-border`, `--danger-text`
  - `--subagent`, `--subagent-soft`
  - `--system`, `--signal`

### Usage guidelines

- Use the root tokens instead of hard-coded colors in component styles.
- Use **accent** tokens for primary actions, focus states, and selected/interactive UI.
- Use **surface** tokens for cards, panels, sidebars, inputs, and code blocks.
- Use **semantic** tokens only for status-dependent UI such as alerts, pills, and timeline markers.
- Prefer `--text` / `--text-muted` for hierarchy instead of introducing extra colors.

### Semantic mapping in the UI

The app currently uses these semantic roles:

- **Primary / interactive:** jade-teal accent
- **Success / assistant / connected:** muted green
- **Warning / tool / waiting:** muted ochre
- **Danger / error / denied:** muted clay-brown
- **Subagent:** softened violet
- **System / signal / raw:** quiet gray-green / neutral tones

### Where tokens are consumed

Most theme usage is centralized in `frontend/src/styles.css` through shared classes such as:

- `.button`
- `.session-link`
- `.panel`, `.page-header`, `.timeline-card`, `.modal-card`
- `.inline-alert.*`
- `.pill-*`
- `.timeline-*`

When adjusting the theme, update the root variables first, then refine any semantic or component-specific styles if needed.
