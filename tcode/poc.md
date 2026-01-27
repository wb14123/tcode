# TCode PoC Implementation Plan

## Objective
Create a minimal PoC to validate the core concept: two tmux windows (display + edit) communicating via Unix socket, using neovim buffers for message navigation and editing, integrated with a real LLM via llm-rs.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      tmux session                           │
│  ┌─────────────────────────┐  ┌─────────────────────────┐  │
│  │    Display Window       │  │     Edit Window         │  │
│  │  (tcode main)           │  │  (tcode edit)           │  │
│  │                         │  │                         │  │
│  │  ┌─────────────────┐    │  │  ┌─────────────────┐    │  │
│  │  │ Neovim Buffer   │    │  │  │ Neovim Buffer   │    │  │
│  │  │ - Shows msgs    │    │  │  │ - Edit prompt   │    │  │
│  │  │ - Streaming     │    │  │  │ - Send on :w    │    │  │
│  │  └─────────────────┘    │  │  └─────────────────┘    │  │
│  └──────────┬──────────────┘  └──────────┬──────────────┘  │
│             │                            │                  │
│             │  Unix Socket               │                  │
│             └──────────┬─────────────────┘                  │
│                        ▼                                    │
│              ┌─────────────────┐                            │
│              │  TCode Server   │                            │
│              │  - Socket IPC   │                            │
│              │  - llm-rs conv  │                            │
│              │  - Msg routing  │                            │
│              └─────────────────┘                            │
└─────────────────────────────────────────────────────────────┘
```

## Components

### 1. TCode Server (`tcode/src/server.rs`)
- Listens on Unix socket (`/tmp/tcode-{session}.sock`)
- Manages ConversationManager from llm-rs
- Routes messages between display/edit clients
- Handles LLM streaming responses

### 2. Display Client (`tcode/src/display.rs`)
- Connects to server socket
- Spawns neovim with custom lua plugin
- Streams messages to neovim buffer via RPC/jobstart
- Handles message formatting (user vs assistant)

### 3. Edit Client (`tcode/src/edit.rs`)
- Connects to server socket
- Spawns neovim for prompt editing
- Sends buffer content to server on save
- Clears buffer after send

### 4. Neovim Plugin (`tcode/lua/tcode.lua`)
- Display: receives messages via stdin, appends to buffer
- Edit: hooks BufWritePost to send content, clear buffer
- Basic syntax highlighting for messages

## Implementation Steps

### Step 1: Create project structure
```
tcode/
├── Cargo.toml
├── src/
│   ├── main.rs          # CLI entry point
│   ├── server.rs        # Socket server + LLM
│   ├── display.rs       # Display window client
│   ├── edit.rs          # Edit window client
│   └── protocol.rs      # ClientMessage enum (ServerMessage reuses llm_rs::Message)
└── lua/
    └── tcode.lua        # Neovim plugin
```

### Step 2: Add Serialize to llm-rs Message (prerequisite)

Update `llm-rs/src/conversation.rs`:
```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageEndStatus {
    Succeeded, Failed, Cancelled, Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    UserMessage {
        msg_id: i32,
        created_at: u64,  // Unix timestamp in milliseconds
        content: Arc<String>,
    },
    AssistantMessageStart {
        msg_id: i32,
        created_at: u64,
    },
    // ... other variants: change Instant -> u64 (millis since epoch)
}
```

Helper to get current timestamp:
```rust
use std::time::{SystemTime, UNIX_EPOCH};

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
```

This change requires updating all places in `conversation.rs` that create messages with `Instant::now()` to use `now_millis()` instead.

### Step 3: Define Protocol (`protocol.rs`)

Use length-delimited JSON over Unix socket. Reuse `llm_rs::conversation::Message` directly:

```rust
use llm_rs::conversation::Message;
use serde::{Serialize, Deserialize};

/// Client -> Server messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Send a user message to the conversation
    SendMessage { content: String },
    /// Subscribe to conversation events (display client sends this)
    Subscribe,
}

/// Server -> Client messages
/// Reuses llm_rs::Message directly - no conversion needed!
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Acknowledgment
    Ack,
    /// Conversation event - directly uses llm_rs::Message
    Event(Message),
    /// Error
    Error { message: String },
}
```

**Transport setup using tokio-util + serde_json:**
```rust
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use futures::{SinkExt, StreamExt};

// Server side
let framed = LengthDelimitedCodec::new().framed(stream);
let (mut sink, mut stream) = framed.split();

// Send message
let json = serde_json::to_vec(&ServerMessage::Event(msg))?;
sink.send(json.into()).await?;

// Receive message
if let Some(Ok(bytes)) = stream.next().await {
    let msg: ClientMessage = serde_json::from_slice(&bytes)?;
}
```

**Benefits:**
- Reuses `llm_rs::Message` directly - no proto duplication
- True bidirectional streaming - server pushes events in real-time
- Simple framing with `LengthDelimitedCodec`
- Type-safe with serde

### Step 4: Implement Server (`server.rs`)
- Parse CLI args (socket path, API key, model)
- Create ConversationManager, start conversation
- Accept Unix socket connections and handle clients:
  ```rust
  let listener = UnixListener::bind(socket_path)?;

  loop {
      let (stream, _) = listener.accept().await?;
      let framed = LengthDelimitedCodec::new().framed(stream);
      let (mut sink, mut stream) = framed.split();
      let conv_client = Arc::clone(&conversation_client);

      tokio::spawn(async move {
          while let Some(Ok(bytes)) = stream.next().await {
              let msg: ClientMessage = serde_json::from_slice(&bytes)?;
              match msg {
                  ClientMessage::Subscribe => {
                      // Subscribe to llm-rs events and forward to client
                      let mut events = conv_client.subscribe();
                      while let Some(Ok(event)) = events.next().await {
                          let resp = ServerMessage::Event((*event).clone());
                          let json = serde_json::to_vec(&resp)?;
                          sink.send(json.into()).await?;
                      }
                  }
                  ClientMessage::SendMessage { content } => {
                      conv_client.send_chat(&content).await?;
                      let json = serde_json::to_vec(&ServerMessage::Ack)?;
                      sink.send(json.into()).await?;
                  }
              }
          }
      });
  }
  ```

### Step 5: Implement Display Client (`display.rs`)
- Connect to Unix socket, send `Subscribe` message
- Spawn neovim with custom lua plugin
- Listen for `ServerMessage::Event(msg)` and pipe to neovim:
  ```rust
  let stream = UnixStream::connect(socket_path).await?;
  let framed = LengthDelimitedCodec::new().framed(stream);
  let (mut sink, mut stream) = framed.split();

  // Send subscribe request
  let json = serde_json::to_vec(&ClientMessage::Subscribe)?;
  sink.send(json.into()).await?;

  // Spawn neovim and get its stdin
  let mut nvim = Command::new("nvim")
      .args(["-c", "lua require('tcode').setup_display()"])
      .stdin(Stdio::piped())
      .spawn()?;
  let mut nvim_stdin = nvim.stdin.take().unwrap();

  // Stream events to neovim
  while let Some(Ok(bytes)) = stream.next().await {
      let msg: ServerMessage = serde_json::from_slice(&bytes)?;
      if let ServerMessage::Event(event) = msg {
          writeln!(nvim_stdin, "{}", format_event(&event))?;
      }
  }
  ```
- Neovim lua appends lines to buffer in real-time

### Step 6: Implement Edit Client
- Connect to socket
- Spawn neovim with edit hooks:
  ```
  nvim -c "lua require('tcode').setup_edit()"
  ```
- Lua plugin: on :w, send content to stdout, clear buffer
- Client reads neovim stdout, sends to server

### Step 7: Neovim Plugin (`lua/tcode.lua`)
```lua
local M = {}

function M.setup_display()
  vim.cmd('enew')
  vim.bo.buftype = 'nofile'
  vim.bo.filetype = 'tcode'
  -- Read from stdin and append to buffer
  vim.fn.jobstart({'cat'}, {
    on_stdout = function(_, data)
      vim.api.nvim_buf_set_lines(0, -1, -1, false, data)
    end
  })
end

function M.setup_edit()
  vim.cmd('enew')
  vim.bo.filetype = 'markdown'
  vim.api.nvim_create_autocmd('BufWritePost', {
    callback = function()
      local lines = vim.api.nvim_buf_get_lines(0, 0, -1, false)
      -- Send to stdout (picked up by client)
      io.stdout:write(table.concat(lines, '\n'))
      io.stdout:flush()
      vim.api.nvim_buf_set_lines(0, 0, -1, false, {})
    end
  })
end

return M
```

### Step 8: CLI Entry Point (`main.rs`)
```
tcode              # Start server + display
tcode edit         # Open edit window
tcode --help       # Show help
```

## Key Files to Create

| File | Purpose |
|------|---------|
| `tcode/Cargo.toml` | Dependencies: llm-rs, tokio, tokio-util, serde, clap |
| `tcode/src/main.rs` | CLI parsing, subcommand dispatch |
| `tcode/src/server.rs` | Socket server, LLM integration |
| `tcode/src/display.rs` | Display client, neovim spawning |
| `tcode/src/edit.rs` | Edit client, neovim spawning |
| `tcode/src/protocol.rs` | ClientMessage/ServerMessage (reuses llm_rs::Message) |
| `tcode/lua/tcode.lua` | Neovim integration plugin |

## Dependencies
```toml
[dependencies]
llm-rs = { path = "../llm-rs" }
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
futures = "0.3"
```

No build.rs needed - simple and direct!

## Testing the PoC

1. Start server: `cargo run -- --api-key $OPENAI_API_KEY`
2. In another tmux pane: `cargo run -- edit`
3. Type message in edit window, :w to send
4. Watch response stream in display window

## Scope Exclusions (for PoC)
- No tool calling
- No sub-agents
- No message branching
- No conversation persistence
- No multiple simultaneous conversations
- No fancy status bars or UI chrome
