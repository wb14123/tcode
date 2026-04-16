# tcode web API design

## Goal

This document defines the HTTP API surface for `tcode-web` based on the PoC in `poc.md`.

It is a product/API contract document, not an implementation plan. It focuses on:

- what endpoints the web server should expose
- how those endpoints map to session files and runtime actions
- which endpoints stream live events vs return current values
- how nested subagents should be addressed

## Design principles

### 1. File-shaped API

The API should closely mirror the session files that already exist under a session directory.

Examples:

- `display.jsonl`
- `session-meta.json`
- `conversation-state.json`
- `status.txt`
- `usage.txt`
- `token_usage.txt`
- tool-call event files
- subagent event/state files

This keeps the web API aligned with the existing session data model.

### 2. Append-only event files are streamed

Append-only event files should be exposed as streaming endpoints.

For the PoC, these are represented as file-like routes ending in `.jsonl`, but the response transport is **Server-Sent Events (SSE)**.

The payload should remain close to the underlying file content instead of being transformed into a new normalized event model.

### 3. Snapshot/value files are fetched normally

Non-append files should be returned with normal `GET` requests.

Examples:

- `session-meta.json`
- `conversation-state.json`
- `status.txt`
- `usage.txt`
- `token_usage.txt`

### 4. Public API routes may be cleaner than exact internal filenames

Some internal files use awkward names such as `tool-call-call_<id>.jsonl`.

The public API should expose those with cleaner stable routes such as:

- `/api/sessions/:sessionId/tool-calls/:toolCallId.jsonl`
- `/api/sessions/:sessionId/tool-calls/:toolCallId/status.txt`

The backend maps those routes to the real session files.

### 5. Flatten nested subagents in public routes

Subagents can contain:

- tool calls
- subagents

The API should **not** use recursively nested URL paths like:

- `/api/sessions/:sessionId/subagents/:parentSubagentId/subagents/:childSubagentId/...`

Instead, all subagents in a session should be addressed with a single flattened namespace:

- `/api/sessions/:sessionId/subagents/:subagentId/...`

This applies even when a subagent is itself inside another subagent.

The parent/child hierarchy should be represented in the subagent data and events, not in the URL shape.

This keeps routes simple while still preserving the true nesting structure.

## Authentication and security

All API endpoints and all SSE endpoints require authentication.

For the PoC, use a single-user login flow:

1. the server starts with a configured password or token
2. the client logs in using that secret
3. the server returns an authenticated session cookie
4. the browser uses that cookie for normal API requests and SSE connections

Security requirements:

- cookie-based authentication
- cookie must be `HttpOnly`
- cookie must be `Secure`
- cookie must be `SameSite=Strict`
- do not put auth tokens in query parameters
- validate request origin for browser requests
- enforce origin checks strictly on all state-changing endpoints
- if the server is exposed remotely, it should be behind HTTPS or a trusted tunnel/proxy
- the remote server binds to localhost by default unless explicitly configured otherwise

## Endpoint groups

The API has four groups:

1. authentication endpoints
2. session collection/lifecycle endpoints
3. file-shaped read/stream endpoints
4. action/proxy endpoints for runtime commands

---

## 1. Authentication endpoints

### `POST /api/auth/login`

Authenticate using the configured password or token.

Request body:

```json
{
  "secret": "..."
}
```

Response:

- success response
- sets authenticated session cookie

### `POST /api/auth/logout`

Clear the authenticated session cookie.

### `GET /api/auth/session`

Return whether the current request is authenticated.

Example response:

```json
{
  "authenticated": true
}
```

---

## 2. Session collection and lifecycle endpoints

### `GET /api/sessions`

List existing sessions for the sidebar.

This should return lightweight summary data derived from session files such as:

- session id
- creation time
- last active time
- current status

Example response:

```json
{
  "sessions": [
    {
      "id": "53hthyc8",
      "created_at": "2026-04-15T23:00:00Z",
      "last_active_at": "2026-04-16T00:00:00Z",
      "status": "Ready"
    }
  ]
}
```

### `POST /api/sessions`

Create a new session.

The request may include an initial prompt.

The client must **not** be able to override the working directory. The working directory comes from where `tcode remote` was started.

Request body:

```json
{
  "initial_prompt": "Help me review this project"
}
```

Response:

```json
{
  "id": "new-session-id"
}
```

Notes:

- `title` is not required for the PoC
- working directory override is not supported
- server/profile defaults are controlled by the backend startup environment

---

## 3. File-shaped read and stream endpoints

## 3.1 Session-level files

### `GET /api/sessions/:sessionId/session-meta.json`

Return the current session metadata snapshot.

### `GET /api/sessions/:sessionId/conversation-state.json`

Return the current conversation state snapshot.

### `GET /api/sessions/:sessionId/status.txt`

Return the current session status text.

### `GET /api/sessions/:sessionId/usage.txt`

Return the current usage stats text.

This is useful for displaying usage information in the web UI.

### `GET /api/sessions/:sessionId/token_usage.txt`

Return the current token usage text.

### `GET /api/sessions/:sessionId/display.jsonl`

Stream the main conversation event file for the session.

Transport:

- SSE

Behavior:

- follows new appended events from `display.jsonl`
- payload stays close to the file-backed event data
- used by the main conversation display

---

## 3.2 Session tool-call files

These endpoints expose top-level tool calls belonging directly to the session.

### `GET /api/sessions/:sessionId/tool-calls/:toolCallId.jsonl`

Stream the append-only event file for a tool call.

Internal file mapping may point to a file like `tool-call-call_<id>.jsonl`, but the public route should use the cleaner `tool-calls/:toolCallId.jsonl` form.

Transport:

- SSE

### `GET /api/sessions/:sessionId/tool-calls/:toolCallId/status.txt`

Return the current tool-call status text.

---

## 3.3 Subagent files

Subagents should be addressed in a flattened session-scoped namespace.

This means that even if a subagent is nested inside another subagent, its API path is still:

- `/api/sessions/:sessionId/subagents/:subagentId/...`

not a recursively nested route.

The true parent/child subagent structure should be represented in the subagent metadata and event data.

### `GET /api/sessions/:sessionId/subagents/:subagentId/session-meta.json`

Return the current metadata snapshot for a subagent.

This should include enough information to identify its parent context if applicable.

### `GET /api/sessions/:sessionId/subagents/:subagentId/conversation-state.json`

Return the current conversation state snapshot for a subagent.

### `GET /api/sessions/:sessionId/subagents/:subagentId/status.txt`

Return the current subagent status text.

### `GET /api/sessions/:sessionId/subagents/:subagentId/token_usage.txt`

Return the current token usage text for the subagent.

### `GET /api/sessions/:sessionId/subagents/:subagentId/display.jsonl`

Stream the append-only event file for a subagent conversation.

Transport:

- SSE

Behavior:

- used by the subagent detail page
- payload remains close to the file-backed event stream

---

## 3.4 Subagent tool-call files

Subagents can have their own tool calls.

Those tool calls should be addressed under the owning subagent:

### `GET /api/sessions/:sessionId/subagents/:subagentId/tool-calls/:toolCallId.jsonl`

Stream the append-only event file for a subagent tool call.

Transport:

- SSE

### `GET /api/sessions/:sessionId/subagents/:subagentId/tool-calls/:toolCallId/status.txt`

Return the current status text for a subagent tool call.

---

## 4. Action and proxy endpoints

These endpoints proxy or trigger runtime actions rather than exposing files directly.

## 4.1 Messaging and conversation control

### `POST /api/sessions/:sessionId/messages`

Send a new user message to the session.

Example request:

```json
{
  "text": "Please summarize this change"
}
```

### `POST /api/sessions/:sessionId/finish`

Finish the current user request.

### `POST /api/sessions/:sessionId/cancel`

Cancel the current conversation or active run.

## 4.2 Tool control

### `POST /api/sessions/:sessionId/tool-calls/:toolCallId/cancel`

Cancel a top-level tool call.

If subagent tool-call cancellation is needed through the web UI, it should also be supported with a parallel subagent-scoped route:

### `POST /api/sessions/:sessionId/subagents/:subagentId/tool-calls/:toolCallId/cancel`

Cancel a tool call owned by a subagent.

## 4.3 Permission APIs

### `GET /api/sessions/:sessionId/permissions`

Return the current permission state, including any pending permission requests relevant to the UI.

### `POST /api/sessions/:sessionId/permissions/resolve`

Approve or deny a pending permission request.

Example request:

```json
{
  "request_id": "...",
  "decision": "approve"
}
```

### `POST /api/sessions/:sessionId/permissions`

Add a permission rule.

### `DELETE /api/sessions/:sessionId/permissions/:permissionId`

Revoke a permission rule.

---

## Streaming semantics

### `.jsonl` endpoints

Routes ending in `.jsonl` represent append-only event files.

For the PoC they should:

- use SSE transport
- stream newly appended events
- stay close to the underlying file event format
- support live UI updates

Examples:

- `/api/sessions/:sessionId/display.jsonl`
- `/api/sessions/:sessionId/tool-calls/:toolCallId.jsonl`
- `/api/sessions/:sessionId/subagents/:subagentId/display.jsonl`
- `/api/sessions/:sessionId/subagents/:subagentId/tool-calls/:toolCallId.jsonl`

### `.json` endpoints

Routes ending in `.json` return the current snapshot value for that resource.

Examples:

- `session-meta.json`
- `conversation-state.json`

### `.txt` endpoints

Routes ending in `.txt` return the current text value for that resource.

Examples:

- `status.txt`
- `usage.txt`
- `token_usage.txt`

---

## Nested subagent addressing rule

Subagents may contain both tool calls and further subagents.

To keep the API stable and simple:

- all subagents are addressed directly under `/api/sessions/:sessionId/subagents/:subagentId/...`
- nested subagents are **not** exposed via recursive paths
- subagent ancestry is represented in data, not in URL nesting

Examples:

Good:

- `/api/sessions/abc/subagents/sub1/display.jsonl`
- `/api/sessions/abc/subagents/sub2/display.jsonl`
- `/api/sessions/abc/subagents/sub2/tool-calls/call9.jsonl`

Not recommended:

- `/api/sessions/abc/subagents/sub1/subagents/sub2/display.jsonl`

This flattened model should be called out explicitly in the API design because subagents can nest recursively.

---

## Endpoints intentionally out of scope for this PoC doc

Do not include the following for now:

- log endpoints for `debug.log`, `stdout.log`, or `stderr.log`
- file diff preview endpoints under `tool-file-preview/`
- tree-view-specific APIs
- multi-user auth or user management APIs

---

## Full endpoint inventory

### Authentication

- `POST /api/auth/login`
- `POST /api/auth/logout`
- `GET /api/auth/session`

### Session collection and lifecycle

- `GET /api/sessions`
- `POST /api/sessions`

### Session file endpoints

- `GET /api/sessions/:sessionId/session-meta.json`
- `GET /api/sessions/:sessionId/conversation-state.json`
- `GET /api/sessions/:sessionId/status.txt`
- `GET /api/sessions/:sessionId/usage.txt`
- `GET /api/sessions/:sessionId/token_usage.txt`
- `GET /api/sessions/:sessionId/display.jsonl`

### Top-level session tool-call endpoints

- `GET /api/sessions/:sessionId/tool-calls/:toolCallId.jsonl`
- `GET /api/sessions/:sessionId/tool-calls/:toolCallId/status.txt`
- `POST /api/sessions/:sessionId/tool-calls/:toolCallId/cancel`

### Subagent endpoints

- `GET /api/sessions/:sessionId/subagents/:subagentId/session-meta.json`
- `GET /api/sessions/:sessionId/subagents/:subagentId/conversation-state.json`
- `GET /api/sessions/:sessionId/subagents/:subagentId/status.txt`
- `GET /api/sessions/:sessionId/subagents/:subagentId/token_usage.txt`
- `GET /api/sessions/:sessionId/subagents/:subagentId/display.jsonl`

### Subagent tool-call endpoints

- `GET /api/sessions/:sessionId/subagents/:subagentId/tool-calls/:toolCallId.jsonl`
- `GET /api/sessions/:sessionId/subagents/:subagentId/tool-calls/:toolCallId/status.txt`
- `POST /api/sessions/:sessionId/subagents/:subagentId/tool-calls/:toolCallId/cancel`

### Session action endpoints

- `POST /api/sessions/:sessionId/messages`
- `POST /api/sessions/:sessionId/finish`
- `POST /api/sessions/:sessionId/cancel`
- `GET /api/sessions/:sessionId/permissions`
- `POST /api/sessions/:sessionId/permissions/resolve`
- `POST /api/sessions/:sessionId/permissions`
- `DELETE /api/sessions/:sessionId/permissions/:permissionId`

---

## Error handling

Suggested status code usage:

- `401 Unauthorized` for missing or invalid authentication
- `403 Forbidden` for rejected origin or denied access
- `404 Not Found` when the session, subagent, tool call, or file-shaped resource does not exist
- `409 Conflict` when an action is invalid for the current runtime state
- `500 Internal Server Error` for unexpected backend failures
