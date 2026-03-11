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
- **Message types**: UserMessage, AssistantMessageStart/End/Chunk, ToolMessageStart/Output/End, SubAgentStart/End/TurnEnd/Continue, and AssistantRequestEnd.

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
  → Single event loop receives Message::UserMessage
    → Calls LLM with message history, streams LLMEvents
      → Events broadcast to all subscribers as Messages
        → On ToolUse: spawn tool tasks (fire-and-forget)
        → On EndTurn with pending tools: wait for tool completions
        → Tool tasks send results back as Message::ToolOutputChunk / ToolMessageEnd
        → Once all pending tools complete: call LLM again with tool results
        → On EndTurn with no pending tools: done, await next user message
  → If user sends a new message while tools are running:
    → Cancel outstanding tools, fill synthetic "cancelled" results
    → Call LLM with the new user message
```

## Subagents

The conversation manager supports spawning subagent conversations — independent conversations that run a task and return the result to the parent. Subagents are **multi-turn**: after completing their initial task they remain idle, allowing the parent to send follow-up messages without losing context.

### How It Works

Three sentinel tools are registered with the LLM:

- **`subagent`** — Takes a `task` (string) and `model` (model ID). The LLM calls this when it wants to delegate work. Returns the result prefixed with `[subagent_id: <conversation_id>]`.
- **`continue_subagent`** — Takes a `conversation_id` and `message`. Sends a follow-up message to an existing idle subagent and returns its response (also prefixed with `[subagent_id: ...]`).
- **`get_subagent_logs`** — Takes a `conversation_id`. Returns the full message history of a subagent for inspection.

Sentinel tools are registered in the LLM's tool schema but intercepted in the conversation loop rather than executing through the normal tool system.

### Execution Flow

**Initial spawn:**
```
Parent conversation: LLM calls subagent tool(task, model)
  → Conversation loop intercepts the tool call
  → Creates new Conversation via ConversationManager::new_conversation(single_turn=true)
  → Broadcasts SubAgentStart { conversation_id, description }
  → Sends task to subagent, collects AssistantMessageChunk text
  → On AssistantRequestEnd: broadcasts SubAgentTurnEnd { response, tokens }
  → Subagent is now idle (conversation loop keeps running, awaiting follow-ups)
  → Inserts "[subagent_id: ...]\n<response>" as ToolResult into parent's message history
  → Parent LLM continues with the subagent's answer
```

**Follow-up (continue):**
```
Parent conversation: LLM calls continue_subagent(conversation_id, message)
  → Looks up existing subagent via ConversationManager::get_conversation()
  → Broadcasts SubAgentContinue { conversation_id, description }
  → Subscribes to new messages only (subscribe_new(), no history replay)
  → Sends follow-up via send_chat()
  → Collects response until AssistantRequestEnd
  → Broadcasts SubAgentTurnEnd { response, tokens }
  → Inserts "[subagent_id: ...]\n<response>" as ToolResult
```

### Status Lifecycle

```
SubAgentStart    → "Running"   (subagent created and working)
SubAgentTurnEnd  → "Idle"      (subagent alive, waiting for follow-up)
SubAgentContinue → "Running"   (subagent resumed with new message)
SubAgentTurnEnd  → "Idle"      (waiting again)
...
(cleaned up on server shutdown)
```

The `SubAgentEnd` message type still exists for terminal shutdown of a subagent but is not used in the normal flow — subagents transition to idle after each turn.

### Cancellation

Conversations support cancellation at multiple granularities:

- **Tool-level**: `ConversationClient::cancel_tool(id)` cancels a single tool's `CancellationToken`.
- **Conversation-level**: `ConversationClient::cancel()` cancels the conversation's cancel token (which cascades to all child tool tokens since they are created as `child_token()`), recursively cancels all registered child subagent conversations, and broadcasts a system warning.

**Cancel token hierarchy:**
```
ConversationClient cancel_token
  ├─ tool "a" token (child_token)
  ├─ tool "b" token (child_token)
  └─ children
       └─ child ConversationClient cancel_token
            ├─ tool "c" token (child_token)
            └─ children
                 └─ grandchild ConversationClient ...
```

**Resumability**: After cancellation, the cancel token is reset (`reset_cancel_token()`) so the conversation can accept new messages. This allows subagents to be resumed via `continue_subagent` after being cancelled.

**User message during tool execution**: When a user sends a new message while tools are still running, the event loop cancels outstanding tools, accumulates any partial results already received, fills synthetic "cancelled" results for remaining tools via `fill_remaining_cancelled()`, and then proceeds with the new user message. This replaces the old `user_interrupted` flag approach with a unified event-loop-based mechanism.

### Nested Subagents

Subagents can spawn their own subagents up to a configurable depth limit controlled by two parameters on `new_conversation()`:

- **`subagent_depth`** — Current nesting level (0 for root conversations).
- **`max_subagent_depth`** — Maximum allowed depth. A subagent at depth `d` receives the `subagent`, `continue_subagent`, and `get_subagent_logs` tools only if `d + 1 < max_subagent_depth`.

```
Root conversation (depth=0, max=3)
  └─ Subagent A (depth=1, max=3) — has subagent tools
       └─ Subagent B (depth=2, max=3) — NO subagent tools (2+1 >= 3)
```

The tcode CLI exposes `--max-subagent-depth` (default: 3) to control this.

### Design Decisions

- **Multi-turn with idle state**: Subagents run with `single_turn=true` — they process one user message, execute any tool calls, and broadcast `AssistantRequestEnd` per turn, but keep their conversation loop running so the parent can resume them with `continue_subagent`. This preserves the subagent's full context across follow-ups without re-sending history.
- **Depth-limited nesting**: Subagents inherit the parent's tools including `subagent`/`continue_subagent`/`get_subagent_logs` when the depth limit allows, enabling recursive delegation. At the deepest allowed level, these tools are excluded to prevent infinite nesting.
- **Context isolation**: Each subagent gets its own conversation with independent message history and token tracking.
- **Model selection**: The LLM chooses which model to use for the subagent from the available models list (included in the tool description).
- **Max iterations**: Subagents are capped at a configurable number of tool-call iterations (`--subagent-max-iterations`, default 50) to prevent runaway loops.

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
