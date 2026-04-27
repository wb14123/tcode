# tcode-web

`tcode-web` is the web UI/backend for `tcode`.

For the current PoC:
- the frontend is built with Lit + Vite + TypeScript
- the Rust server serves the built frontend from the same origin
- auth uses the existing cookie-based login flow

## Build

There are two frontend-serving modes:

- **Development/default builds** read static files from `tcode-web/frontend/dist` at runtime. This keeps normal `cargo check`, `cargo build`, and `cargo run` fast and does not require Node.js unless you want to open the web UI.
- **Bundled builds** compile `tcode-web/frontend/dist` into the `tcode` binary with the `bundled-frontend` Cargo feature. Release and source-install builds use this mode so installed binaries do not depend on the source checkout path.

### Build the frontend

From the repo root:

```bash
cd tcode-web/frontend
npm ci
npm run build
```

This writes the production bundle to:

```text
tcode-web/frontend/dist
```

### Development Rust build/check

From the repo root:

```bash
cargo check -p tcode-web
cargo build -p tcode
```

If you run the web backend from a development/default build, the Rust server serves the filesystem `dist` directory above. If `tcode-web/frontend/dist` is missing, the backend still runs but frontend browser routes return `404`.

### Self-contained bundled build

For a release-like binary that can be moved to another machine without `frontend/dist`, build the frontend first, then enable the feature through the top-level `tcode` crate:

```bash
cd tcode-web/frontend
npm ci
npm run build
cd ../..
cargo build -p tcode --features tcode/bundled-frontend
```

Use the same feature with release builds:

```bash
cargo build --release --features tcode/bundled-frontend
```

The GitHub release workflow and `install-from-source.sh` both follow this pattern.

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

- Development/default builds serve `tcode-web/frontend/dist` from the filesystem. Rebuild with `npm run build` after frontend changes before restarting or refreshing the Rust-served app.
- Bundled builds serve embedded files from the binary and do not need `frontend/dist` at runtime.
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
