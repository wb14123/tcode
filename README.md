# llm-rs

A Rust workspace for building LLM-powered coding agents. The primary application is **tcode**, a terminal-based coding agent (similar to Claude Code or Codex) that leverages neovim and tmux for its UI.

## Workspace Structure

```
llm-rs/               Root workspace
├── llm-rs/            Core LLM library (provider abstraction, conversations, tool system)
├── llm-rs-macros/     Proc macros for tool definition (#[tool] attribute)
├── tools/             Built-in tools (web fetch, web search via headless Chrome)
└── tcode/             Terminal coding agent application (server + neovim/tmux clients)
```

## Architecture Overview

```
┌──────────────────────────────────────────────────────┐
│  tcode (Application)                                 │
│  Server-client architecture over Unix sockets        │
│  UI via neovim buffers + tmux panes                  │
│                                                      │
│  ┌─────────┐  ┌──────────┐  ┌───────────────────┐   │
│  │ Display  │  │  Edit    │  │  Tool Call Viewer │   │
│  │ (neovim) │  │ (neovim) │  │  (neovim)         │   │
│  └────┬─────┘  └────┬─────┘  └───────────────────┘   │
│       │ JSONL files  │ Unix socket                    │
│       └──────┬───────┘                                │
│              ▼                                        │
│         Server Process                                │
│              │                                        │
├──────────────┼────────────────────────────────────────┤
│              ▼                                        │
│  llm-rs (Core Library)                                │
│  ├─ ConversationManager (multi-round chat loop)      │
│  │   └─ Multi-turn subagents (resumable, depth-limited)│
│  ├─ LLM trait (provider-agnostic streaming interface)│
│  │   ├─ OpenAI impl (Responses API)                  │
│  │   ├─ OpenRouter impl (Chat Completions API)       │
│  │   └─ Claude impl (Messages API with OAuth)        │
│  └─ Tool system (streaming execution with timeouts)  │
│                                                      │
│  tools (Built-in Tools)                              │
│  ├─ web_fetch (page content extraction)              │
│  └─ web_search (Kagi search)                         │
│                                                      │
│  llm-rs-macros                                       │
│  └─ #[tool] proc macro for tool definitions          │
└──────────────────────────────────────────────────────┘
```

## Key Design Decisions

- **Terminal-native**: Uses neovim and tmux rather than building a custom GUI. Users get familiar keybindings and extensibility for free.
- **Server-client over Unix sockets**: The server manages the conversation and writes JSONL event files. Clients (display, edit, tool-call viewer) are separate neovim processes that read those files. This allows flexible multi-pane layouts via tmux.
- **Streaming everywhere**: LLM responses and tool outputs stream incrementally through async Rust streams for low-latency feedback.
- **Provider-agnostic LLM trait**: The `LLM` trait abstracts providers. Currently implemented for OpenAI (Responses API), OpenRouter (Chat Completions API), and Claude (Messages API with OAuth). Adding new providers means implementing one trait.
- **Macro-based tool definitions**: The `#[tool]` proc macro generates JSON schema, deserialization, and `Tool` construction from a plain function signature.

## Building

```bash
cargo check           # Quick type checking
cargo build           # Debug build
cargo test            # Run tests
```

## Running tcode

```bash
# Inside a tmux session - starts server + display + edit panes
cargo run -p tcode

# Or start components separately
cargo run -p tcode -- serve     # Server only
cargo run -p tcode -- edit      # Editor pane (connects to running server)
cargo run -p tcode -- display   # Display pane (connects to running server)
```

Session data is stored at `~/.tcode/sessions/{session_id}/`.

## Crate Details

See each crate's README for more details:

- [llm-rs/](llm-rs/) - Core LLM library
- [llm-rs-macros/](llm-rs-macros/) - Procedural macros
- [tools/](tools/) - Built-in tools
- [tcode/](tcode/) - Terminal application
