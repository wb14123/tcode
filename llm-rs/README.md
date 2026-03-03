# llm-rs (Core Library)

Core library providing LLM interaction primitives: a provider-agnostic LLM trait, a conversation manager for multi-round chat with tool use, and a streaming tool execution system.

## Modules

### `llm` - LLM Provider Abstraction

The `LLM` trait defines a provider-agnostic interface for chat completions with streaming and tool support.

- **`LLM` trait**: `register_tools()` and `chat()` methods. `chat()` returns a `Stream<Item = LLMEvent>`.
- **`LLMEvent`**: MessageStart, TextDelta, ThinkingDelta, ToolCall, MessageEnd (with stop reason and token usage), Error.
- **`LLMMessage`**: Enum of System, User, Assistant (with tool calls), and ToolResult messages.
- **`ChatOptions`**: Reasoning effort/budget controls.
- **OpenAI implementation** (`openai.rs`): Uses the Responses API (`/v1/responses`). Handles SSE streaming, function calling, and reasoning tokens.
- **OpenRouter implementation** (`openrouter.rs`): Uses Chat Completions API for OpenRouter and compatible providers.
- **Claude implementation** (`claude.rs`): Uses Anthropic Messages API with OAuth authentication. See [Claude OAuth Authentication](#claude-oauth-authentication) below.

### `conversation` - Conversation Manager

Manages multi-round LLM conversations with automatic tool execution loops.

- **`ConversationManager`**: Creates and manages multiple concurrent conversations.
- **`Conversation`**: The core loop - sends messages to LLM, processes responses, executes tool calls, and continues until the LLM returns EndTurn.
- **`ConversationClient`**: Public API handle for sending user messages (`send_chat()`) and subscribing to conversation events via broadcast channel.
- **Message types**: UserMessage, AssistantMessageStart/End/Chunk, ToolMessageStart/Output/End, and variants for sub-agents.

### `tool` - Tool System

Streaming tool execution with timeout support.

- **`Tool`**: Name, description, JSON schema (auto-generated), timeout, and an async handler function.
- **`ToolOutputStream`**: `Pin<Box<dyn Stream<Item = String>>>` - tools stream their output incrementally.
- **`TimeoutStream`**: Wraps a tool's output stream to enforce a total execution timeout.

Tools are typically defined using the `#[tool]` macro from `llm-rs-macros`, not constructed manually.

## Reasoning / Thinking Tokens

Models like OpenAI's o1/o3 and DeepSeek R1 produce reasoning tokens before their final response. This library handles them as follows:

**Streaming Display**: Reasoning is streamed via `ThinkingDelta` events for real-time display. Token usage is tracked separately in `MessageEnd.reasoning_tokens`.

**Multi-turn Persistence**:

| Provider | Reasoning Persisted | Notes |
|----------|---------------------|-------|
| OpenRouter | ✅ Yes | Passed back via `reasoning_details` in Chat Completions format |
| OpenAI | ❌ No | Responses API requires specific item ordering that's difficult to reconstruct |

For OpenAI, the model works normally but loses visibility into its previous chain of thought. To enable full reasoning persistence with OpenAI, use server-managed conversations (`store=true` + `previous_response_id`).

## Conversation Flow

```
User sends message via ConversationClient::send_chat()
  → Conversation loop calls LLM with message history
    → LLM streams back LLMEvents
      → Events broadcast to all subscribers as Messages
        → On ToolUse: execute tool, collect output, loop back to LLM
        → On EndTurn: done, await next user message
```

## Subagents

The conversation manager supports spawning subagent conversations — independent single-turn conversations that run a task and return the result to the parent.

### How It Works

Two sentinel tools are registered with the LLM:

- **`subagent`** — Takes a `task` (string) and `model` (model ID). The LLM calls this when it wants to delegate work.
- **`get_subagent_logs`** — Takes a `conversation_id`. Returns the full message history of a completed subagent for inspection.

Sentinel tools are registered in the LLM's tool schema but intercepted in the conversation loop rather than executing through the normal tool system.

### Execution Flow

```
Parent conversation: LLM calls subagent tool(task, model)
  → Conversation loop intercepts the tool call
  → Creates new Conversation via ConversationManager::new_conversation(single_turn=true)
  → Broadcasts SubAgentStart { conversation_id, description }
  → Sends task to subagent, collects AssistantMessageChunk text
  → On AssistantRequestEnd: broadcasts SubAgentEnd { response, tokens }
  → Inserts response as ToolResult into parent's message history
  → Parent LLM continues with the subagent's answer
```

### Design Decisions

- **Single-turn**: Subagents run with `single_turn=true` — they process one user message, execute any tool calls, and exit after `AssistantRequestEnd`.
- **Tool inheritance**: Subagents inherit the parent's tools except `subagent` and `get_subagent_logs`, preventing recursive spawning.
- **Context isolation**: Each subagent gets its own conversation with independent message history and token tracking.
- **Model selection**: The LLM chooses which model to use for the subagent from the available models list (included in the tool description).
- **Max iterations**: Subagents are capped at 20 tool-call iterations to prevent runaway loops.

## Examples

- `examples/openai_chat.rs` - Simple streaming chat with reasoning
- `examples/openai_tools.rs` - Tool calling demonstration
- `examples/conversation.rs` - Interactive multi-round conversation with tools
- `examples/openrouter_reasoning.rs` - Reasoning with OpenRouter
- `examples/claude_tools.rs` - Claude with tool calling via OAuth

## Claude OAuth Authentication

The Claude implementation uses OAuth tokens from Claude Pro/Max subscriptions (not API keys). These tokens have special requirements enforced by Anthropic.

### Obtaining an OAuth Token

Use the `tcode claude-auth` command to obtain OAuth tokens via the PKCE flow:

```bash
cargo run -p tcode -- claude-auth
```

This opens a browser for authentication and returns access/refresh tokens.

### OAuth Token Requirements

Claude Code OAuth tokens require specific request signatures to work. The implementation handles these automatically:

1. **System Prompt Prefix**: Every request must include this exact prefix as the first system block:
   ```
   You are Claude Code, Anthropic's official CLI for Claude.
   ```

2. **Beta Headers**: The `anthropic-beta` header must include:
   - `claude-code-20250219` - Identifies as Claude Code
   - `oauth-2025-04-20` - OAuth beta feature
   - `interleaved-thinking-2025-05-14` - Thinking support
   - `fine-grained-tool-streaming-2025-05-14` - Tool streaming

3. **Tool Name Prefix**: All tool names must be prefixed with `mcp_` (e.g., `mcp_get_weather`). The prefix is stripped when returning tool calls to the caller.

4. **Additional Headers**:
   - `x-app: cli`
   - `anthropic-dangerous-direct-browser-access: true`
   - `User-Agent: claude-cli/2.1.2 (external, cli)`

5. **URL Parameter**: Requests must include `?beta=true` query parameter.

6. **System Format**: The system prompt must use array format:
   ```json
   "system": [{"type": "text", "text": "..."}]
   ```

### Usage Example

```rust
use llm_rs::llm::{Claude, LLM, LLMMessage, ChatOptions};

let claude = Claude::new(access_token);
let messages = vec![
    LLMMessage::User("Hello!".to_string()),
];

let stream = claude.chat("claude-sonnet-4-20250514", &messages, &ChatOptions::default());
```

### References

- OAuth authentication flow based on [opencode-anthropic-auth](https://github.com/anomalyco/opencode-anthropic-auth)
- See [issue #12](https://github.com/anomalyco/opencode-anthropic-auth/issues/12) and [issue #33](https://github.com/anomalyco/opencode-anthropic-auth/issues/33) for details on OAuth requirements
