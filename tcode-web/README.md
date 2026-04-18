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
