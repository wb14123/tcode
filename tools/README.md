# tools

Built-in tool implementations for the LLM agent. Tools communicate with a separate `browser-server` process for web interaction.

## Tools

### `web_fetch`

Fetches a web page and extracts its main content. Returns clean, readable HTML suitable for LLM consumption.

- Smart content-type detection: HTML pages go through browser-server (accessibility tree extraction), plaintext is fetched directly via reqwest
- 300 second timeout
- **Requires permission**: Before fetching, prompts the user to approve access to the target hostname (e.g., `example.com`). Once approved for a session or project, subsequent fetches to the same hostname proceed without prompting.

### `web_search`

Performs a web search and returns formatted results with titles, URLs, and snippets. The search engine is configurable via the `--search-engine` CLI flag (default: Google). Supported engines: **Kagi** and **Google**.

- Delegates to browser-server for search extraction
- 300 second timeout
- Kagi engine requires a Kagi session (use `tcode browser` to log in)
- Google engine works without authentication

### `current_time`

Returns the current date and time.

### `edit`

Replaces text in an existing file by string match.

- Matching is attempted byte for byte first, unchanged from before. The retry can never match a file that has no carriage return anywhere, so such a file behaves exactly as it always has.
- Only if the exact search finds nothing is the search retried with the search text and the replacement text converted to Windows line endings: every `\n` not already preceded by `\r` becomes `\r\n`. A search text written with Unix endings therefore edits a Windows-style file, and the lines the edit inserts carry the endings the file uses.
- A search that matches more than once without `replace_all` is reported as ambiguous immediately and is never retried.
- Text outside the replaced region is copied byte for byte.
- A replacement that would not change any byte of the file is reported as a no-op and the file is left untouched.

Known limitations, each deliberate and pre-existing behaviour rather than a regression:

- A search that spans a region where the file's own endings are mixed is not found: each attempt requires one style throughout the matched region.
- A search text that already contains `\r\n` against a file that uses `\n` is not found. The reverse direction is deliberately not attempted.
- A search text with no line break at all matches on the exact attempt, and the replacement is written as supplied, so a single-line search that inserts lines into a Windows-style file writes Unix endings. Nothing is inspected on the exact attempt.
- If a file contains the same text with both styles, the exact attempt wins and replaces the occurrence with Unix endings, rather than reporting an ambiguity.
- A search text that does carry Windows endings and matches on the exact attempt is used as supplied, so a replacement written with Unix endings stays Unix.

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

### Adding Permission Checks

Tools that access sensitive resources should request user permission via `ToolContext`:

```rust
#[tool]
fn my_tool(
    #[context] ctx: ToolContext,
    resource: String,
) -> impl Stream<Item = Result<String, String>> {
    async_stream::try_stream! {
        if !ctx.permission.ask_permission(
            &format!("Allow access to {}?", resource),
            "resource_type",
            &resource,
        ).await {
            yield Err("Permission denied".to_string());
            return;
        }
        // ... proceed with tool logic
    }
}
```

The `ScopedPermissionManager` handles deduplication — concurrent calls with the same `(tool, key, value)` share a single prompt. Permissions can be granted for one use, the current session, or the project (persisted to disk).
