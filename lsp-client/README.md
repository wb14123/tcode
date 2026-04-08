# lsp-client

LSP client library for providing code intelligence to the LLM agent. Manages LSP server lifecycles and communicates over JSON-RPC/stdio, reusing the user's Neovim LSP configuration so no manual server setup is required.

## Architecture

```
tcode (startup)
  |
  +-- extract_config_from_nvim()  // headless nvim discovers installed servers
  |
  +-- LspManager::new(config, root_dir)
        |
        +-- LspServer::start()    // spawns server process, performs initialize handshake
              |
              +-- LspTransport    // JSON-RPC framing over stdin/stdout
```

`tcode` creates an `LspManager` at startup and passes it (via `Arc`) to the `tools` crate, which exposes it as the `LSP` tool.

## Advertised Client Capabilities

The client advertises support for:

- `textDocument/definition` (go-to-definition)
- `textDocument/references` (find references)
- `textDocument/hover` (type info, documentation)
- `textDocument/documentSymbol` (hierarchical symbols in a file)
- `textDocument/implementation` (go-to-implementation)
- `textDocument/callHierarchy` (incoming/outgoing calls)
- `workspace/symbol` (project-wide symbol search)
- `window/workDoneProgress` (progress tracking)

## Modules

### `config`

LSP configuration extraction from Neovim. Runs `nvim --headless` with an embedded Lua script that queries `vim.lsp.config` and `vim.filetype.match` to produce a JSON manifest of:

- Server names, commands, filetypes, root markers
- Server settings and init options
- File extension to filetype mappings

Falls back to an empty config on any failure (nvim not installed, timeout, parse error).

### `manager`

`LspManager` owns the config and a map of running server instances. Key behaviors:

- **Lazy start**: `get_or_start_server(filetype)` spawns a server on first use for a given filetype
- **Root detection**: walks up from project dir looking for root markers (e.g. `Cargo.toml`, `go.mod`)
- **Pre-warming**: `pre_warm()` detects project type from marker files and eagerly starts relevant servers
- **Shutdown**: `shutdown_all()` sends the LSP shutdown/exit sequence to all running servers

### `server`

`LspServer` wraps a single running LSP server process. Handles:

- The `initialize` / `initialized` handshake
- Typed request/response via `request::<R>(params)` using `lsp-types` trait bounds
- Typed notifications via `notify::<N>(params)`
- Convenience methods: `open_file()`, `close_file()`
- Server capability and progress inspection
- rust-analyzer special-casing: injects `checkOnSave: false` to avoid heavy background checks

### `transport`

`LspTransport` implements the JSON-RPC over stdio protocol:

- Content-Length framed message reading/writing
- Background reader task that dispatches responses to waiting `oneshot` channels
- Auto-replies to server-initiated requests (e.g. `window/workDoneProgress/create`)
- `ProgressTracker` for monitoring server indexing/loading state
- 30-second request timeout with automatic cleanup
- Clean shutdown with process kill on drop

## Dependencies

| Crate | Purpose |
|-------|---------|
| `lsp-types` | LSP protocol type definitions |
| `tokio` | Async runtime, process spawning, IO |
| `serde` / `serde_json` | JSON-RPC serialization |
| `parking_lot` | Fast synchronous mutexes (progress tracker) |
| `url` | File path to URI conversion |
| `anyhow` | Error handling |
| `tracing` | Structured logging |
