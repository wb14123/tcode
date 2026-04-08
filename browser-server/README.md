# browser-server

> For browser setup instructions, see [docs/06-browser.md](../docs/06-browser.md).

A standalone headless Chrome server that exposes `web_search` and `web_fetch` as REST APIs over a Unix socket or TCP.

## Architecture

```
tcode instance 1 ──┐
tcode instance 2 ──┤──▶ browser-server (Unix socket or TCP) ──▶ headless Chrome
other programs   ──┘                                              ~/.tcode/chrome/
```

Multiple clients share a single browser-server process. The server manages Chrome lifecycle, tab pooling, and idle shutdown automatically.

## Endpoints

### POST /web_search

Search the web using a configurable search engine (Kagi or Google) and return results as formatted text.

**Request:**
```json
{ "query": "rust async patterns", "engine": "kagi" }
```

The `engine` field is optional and defaults to `"kagi"`. Valid values: `"kagi"`, `"google"`.

**Response:**
```json
{ "content": "Title: Async in Rust\nURL: https://example.com/async\nA guide to async patterns...\n" }
```

### POST /web_fetch

Fetch a web page and extract content as a compact accessibility tree. Uses Chrome's CDP Accessibility API to produce a structured text representation that is much more token-efficient than HTML.

**Request:**
```json
{ "url": "https://example.com/article" }
```

**Response:**
```json
{ "content": "heading \"Article Title\" level: 1\n  paragraph\n    Some content here...\n    link \"Read more\" url: /more\n" }
```

### GET /health

Health check endpoint.

**Response:**
```json
{ "status": "ok" }
```

### Error format

All endpoints return errors as:
```json
{
  "error": {
    "message": "description of what went wrong",
    "type": "browser_error"
  }
}
```

## CLI Usage

```bash
# Unix socket mode (default, no auth)
browser-server
browser-server --socket /tmp/my-browser.sock

# TCP mode (bearer token auth required)
browser-server --bind 0.0.0.0:8090 --token-file tokens.json

# With idle timeout (auto-exit after 5 minutes of inactivity)
browser-server --idle-timeout 300

# Launch visible Chrome to log in to services (e.g., Kagi for web search)
browser-server browser
```

### Subcommands

#### `browser-server browser`

Launches a visible (non-headless) Chrome window with the persistent profile at `~/.tcode/chrome/`. Use this to log in to services (e.g., Kagi for web search) before running the server. The command blocks until the browser window is closed. Fails with an error if the profile is already locked by a running browser-server instance.

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--socket <path>` | `~/.tcode/browser-server.sock` | Unix socket path |
| `--bind <addr>` | (none) | TCP bind address; enables bearer token auth |
| `--token-file <path>` | `~/.config/browser-server/tokens.json` | Token file for TCP auth |
| `--idle-timeout <secs>` | (none) | Exit after N seconds with no requests |

### Token file format

When using `--bind` for TCP mode, create a JSON file with an array of valid bearer tokens:

```json
["token-abc-123", "token-def-456"]
```

## Logging

When run standalone, logs go to stderr.

When auto-started by tcode, logs are written to `~/.tcode/browser-server.log`.

## How tcode uses it

When tcode starts, it automatically manages a browser-server instance:

1. Checks if `~/.tcode/browser-server.sock` has a healthy server
2. If yes, reuses it (multiple tcode sessions share one server)
3. If no, spawns `browser-server --socket ... --idle-timeout 300`
4. The server exits on its own after 5 minutes of inactivity

Logs are at `~/.tcode/browser-server.log`.

For remote browser-server access:
```bash
tcode --browser-server-url http://host:8090 --browser-server-token xxx
```

## Internal Modules

### `browser`

Shared headless Chrome management with tab pooling. Launches Chrome with a persistent profile at `~/.tcode/chrome/`, handles navigation and page load waiting via `wait-for-idle.js`. Features automatic crash recovery and idle browser shutdown.

### `web_search`

Web search extraction. Supports multiple search engines (Kagi and Google). Navigates to the selected engine, extracts and formats the results into a text representation.

### `web_fetch`

Page content extraction. Loads pages in Chrome and uses Chrome's CDP Accessibility Tree API to produce a compact, structured text representation.

## Shared Types

The crate exports request/response types via `lib.rs` that are used by both the server handlers and the `tools` crate's HTTP client:

- `WebSearchRequest` / `WebSearchResponse`
- `WebFetchRequest` / `WebFetchResponse`
- `SearchEngineKind` (enum: `Kagi`, `Google`)
- `ErrorResponse` / `ErrorDetail`
- `HealthResponse`
