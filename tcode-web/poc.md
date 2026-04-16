# PoC for tcode web

## Goal

Provide a web interface for tcode. Make it easier to use on mobile phones for causal tasks.

Security is a top priority.

## Design 

It have 3 parts:

1. tcode server: like what the existing tcode server do: listen on Conversation events and write them to display files under session dir. Just call existing code.
2. tcode web-server: the backend of tcode web.
  a. It provides HTTP APIs and SSE streams for session events. For example, `/api/sessions/:id/events` streams updates from the session files such as `display.jsonl`.
  b. It proxies the existing in-process tcode server handlers for the current supported commands:
    - send message
    - finish user request
    - cancel tool
    - cancel conversation
    - get permission state
    - resolve permission
    - add permission
    - revoke permission
  c. In addition to that, it provides APIs to list existing sessions, and be able to create a new session.
  d. The web backend is the single owner of session runtimes. Each session id has at most one in-process tcode runtime, and all browser clients for that session attach to the same backend-owned runtime.
  e. The backend may manage multiple sessions at the same time. Each session keeps its own session dir and server socket, while sharing global tool/browser config for the PoC.
3. tcode frontend: the frontend to listen on web-server events, render it to user, and call tcode web-server for things like sending a new message.

## Tech Stack

### Backend

- Rust
- HTTP APIs and SSE streams
- Reuse the existing in-process tcode server and session runtime where practical

### Frontend

- Lit for UI components and rendering
- Vite for development and bundling
- TypeScript for application code
- Plain CSS with CSS custom properties for shared theme tokens

### Rationale

The frontend should remain lightweight for the PoC. The backend already exposes structured APIs and SSE streams, so a small component-based frontend is a better fit than mixing multiple frontend paradigms.

Do not use htmx or Alpine in this PoC.

Do not use Tailwind CSS by default. Plain CSS is sufficient for a consistent UI theme and aligns better with Lit's component model.

Re-evaluate the frontend framework choice only if the web UI grows substantially in scope or complexity.

## User Interface

The user interface is mainly a sidebar on the left, and a main display on the right.

### Sidebar

The sidebar on the left should have all the chat sessions. Click one of them will open the conversation. The sidebar should also have an option to open a new conversation.

### Main Display

The main display is on the right of the sidebar. The UI is like what the current tcode main display: it shows the messages in main conversation. For events like tool call or subagent, it shows a brief overview in the UI, and be able to click into a detailed page.

### Detailed tool call page

Clicked from the main display, open a totally new page. Like the current tcode tool call detail window, show all the tool call details.

### Detailed subagent page

Clicked from the main display, open a subanget page. Like the current tcode subagent window, show it like the main conversation.

### Permission Approval

Popup the permission approval request. Now need for preview the write/edit diff for now.

### Tree View

Do not need to implement the current tcode tree UI.

## CLI Interface

`tcode -p <profile> remote --port <port> --password <password>` starts a new remote server. The remote server should bind to localhost by default, and require explicit configuration to listen on non-local addresses.

## Security

The server should always be protected by authentication. Do not need to have user management for now.

For the PoC, use a single-user login flow:
1. The server starts with a configured password or token.
2. The client logs in with that secret using a login API.
3. The server returns an authenticated session cookie.
4. The browser uses that cookie for normal API calls and SSE connections.

The authentication cookie should be HttpOnly, Secure, and SameSite=Strict.

Do not put authentication tokens in URL query parameters.

All API and SSE endpoints should require authentication.

For browser requests, validate request origin, with strict enforcement on all state-changing APIs.

If the server is exposed remotely, it should be used behind HTTPS or a trusted tunnel/proxy.
