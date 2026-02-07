# tools

Built-in tool implementations for the LLM agent. All tools use headless Chrome for web interaction.

## Tools

### `web_fetch`

Fetches a web page and extracts its main content using Mozilla's Readability.js algorithm. Returns clean, readable text suitable for LLM consumption.

- Uses headless Chrome to render JavaScript-heavy pages
- Applies Readability.js for content extraction (strips navigation, ads, etc.)
- 300 second timeout

### `web_search`

Performs a web search via Kagi and returns formatted results with titles, URLs, and snippets.

- Uses headless Chrome to query Kagi search
- Parses results including sub-results
- 300 second timeout
- Requires Kagi session (use `tcode browser` to log in)

## Internal Modules

### `browser`

Shared headless Chrome management. Launches Chrome with a persistent profile at `~/.tcode/chrome/`, handles navigation and page load waiting. Strips `LD_PRELOAD` to avoid proxy issues with Chrome subprocesses.

## Adding New Tools

1. Create a new module under `src/`
2. Define your tool function with the `#[tool]` attribute macro
3. Export the `{name}_tool()` constructor from `lib.rs`
4. Register it in the server (`tcode/src/server.rs`)
