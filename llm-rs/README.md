# llm-rs (Core Library)

Core library providing LLM interaction primitives: a provider-agnostic LLM trait, a conversation manager for multi-round chat with tool use, and a streaming tool execution system.

## Modules

### `llm` - LLM Provider Abstraction

The `LLM` trait defines a provider-agnostic interface for chat completions with streaming and tool support.

- **`LLM` trait**: `register_tools()` and `chat()` methods. `chat()` returns a `Stream<Item = LLMEvent>`.
- **`LLMEvent`**: MessageStart, TextDelta, ThinkingDelta, ToolCall, MessageEnd (with stop reason and token usage), Error.
- **`LLMMessage`**: Enum of System, User, Assistant (with tool calls), and ToolResult messages.
- **`ChatOptions`**: Reasoning effort/budget controls.
- **OpenAI implementation** (`openai.rs`): Works with OpenAI, OpenRouter, and any OpenAI-compatible API. Handles SSE streaming, function calling, and reasoning/thinking tokens.

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

## Conversation Flow

```
User sends message via ConversationClient::send_chat()
  → Conversation loop calls LLM with message history
    → LLM streams back LLMEvents
      → Events broadcast to all subscribers as Messages
        → On ToolUse: execute tool, collect output, loop back to LLM
        → On EndTurn: done, await next user message
```

## Examples

- `examples/openai_chat.rs` - Simple streaming chat with reasoning
- `examples/openai_tools.rs` - Tool calling demonstration
- `examples/conversation.rs` - Interactive multi-round conversation with tools
- `examples/openrouter_reasoning.rs` - Reasoning with OpenRouter
