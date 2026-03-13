# tools

Built-in tool implementations for the LLM agent. Tools communicate with a separate `browser-server` process for web interaction.

## Tools

### `web_fetch`

Fetches a web page and extracts its main content. Returns clean, readable HTML suitable for LLM consumption.

- Smart content-type detection: HTML pages go through browser-server (Readability.js extraction), plaintext is fetched directly via reqwest
- 300 second timeout

### `web_search`

Performs a web search via Kagi and returns formatted results with titles, URLs, and snippets.

- Delegates to browser-server for Kagi search extraction
- Parses results including sub-results
- 300 second timeout
- Requires Kagi session (use `tcode browser` to log in)

### `current_time`

Returns the current date and time.

## Browser Client

The `browser_client` module provides an HTTP client for communicating with `browser-server`:

```rust
use tools::browser_client::{BrowserClient, set_global_client};

// Unix socket mode (used by tcode auto-start)
set_global_client(BrowserClient::unix(socket_path));

// TCP mode (for remote browser-server)
set_global_client(BrowserClient::tcp(url, token));
```

The global client must be initialized before `web_fetch` or `web_search` tools are used. tcode handles this automatically.

## Adding New Tools

1. Create a new module under `src/`
2. Define your tool function with the `#[tool]` attribute macro
3. Export the `{name}_tool()` constructor from `lib.rs`
4. Register it in the server (`tcode/src/server.rs`)
