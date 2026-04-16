# tcode-web milestones and tickets

This file turns `poc.md` and `api.md` into an implementation checklist.

## Scope guardrails

- Keep the PoC focused on a single-user authenticated web UI.
- Reuse the existing in-process tcode runtime where practical.
- Keep the public API file-shaped and close to session files.
- Use flattened subagent routes under `/api/sessions/:sessionId/subagents/:subagentId/...`.
- Out of scope for now: tree view, log endpoints, tool file diff preview endpoints, and multi-user auth.

## Milestone 1 — Backend bootstrap and security baseline

- [ ] Add a `tcode remote` startup path that launches the web backend, accepts `--port` and `--password`, and binds to localhost by default.
  - Verify: start the server with `tcode -p <profile> remote --port <port> --password <secret>` and confirm it only listens on `127.0.0.1` unless an explicit non-local bind option is added.
  - Verify: open `http://127.0.0.1:<port>/api/auth/session` before login and confirm the request is rejected or reports `authenticated: false`.

- [ ] Implement `POST /api/auth/login`, `POST /api/auth/logout`, and `GET /api/auth/session` using a cookie-based auth flow.
  - Verify: `POST /api/auth/login` with the correct secret succeeds and sets a session cookie.
  - Verify: `GET /api/auth/session` returns `{ "authenticated": true }` after login.
  - Verify: `POST /api/auth/logout` clears the cookie and `GET /api/auth/session` no longer reports an authenticated session.

- [ ] Set secure cookie attributes for the auth cookie: `HttpOnly`, `Secure`, and `SameSite=Strict`.
  - Verify: inspect the `Set-Cookie` response header and confirm all three attributes are present.
  - Verify: confirm auth is cookie-based and no token appears in URL query parameters.

- [ ] Enforce authentication on all API and SSE endpoints.
  - Verify: call a protected JSON endpoint without logging in and confirm it returns `401 Unauthorized`.
  - Verify: attempt to connect to a protected `.jsonl` SSE endpoint without logging in and confirm the connection is rejected.

- [ ] Validate browser request origin and strictly enforce origin checks on all state-changing routes.
  - Verify: send a state-changing request with an allowed `Origin` header and confirm it succeeds when authenticated.
  - Verify: repeat with a disallowed `Origin` header and confirm it returns `403 Forbidden`.

## Milestone 2 — Session lifecycle and session-level file endpoints

- [ ] Implement `GET /api/sessions` to return sidebar-friendly session summaries with id, creation time, last active time, and status.
  - Verify: create or seed multiple sessions, call `GET /api/sessions`, and confirm the response contains one summary object per session.
  - Verify: confirm the list is sufficient to render the sidebar without opening each session.

- [ ] Implement `POST /api/sessions` to create a new session, optionally accepting `initial_prompt`, while refusing any client-supplied working directory override.
  - Verify: `POST /api/sessions` returns a new session id.
  - Verify: create a session with `initial_prompt` and confirm the new session begins processing that prompt.
  - Verify: try to pass a working-directory override and confirm the backend ignores or rejects it.

- [ ] Implement session snapshot endpoints: `session-meta.json`, `conversation-state.json`, `status.txt`, `usage.txt`, and `token_usage.txt`.
  - Verify: call each endpoint for an existing session and confirm each route returns the current snapshot/value instead of a stream.
  - Verify: update a session state and confirm a fresh `GET` reflects the latest values.

- [ ] Implement `GET /api/sessions/:sessionId/display.jsonl` as an SSE stream backed by `display.jsonl`.
  - Verify: connect to the SSE endpoint, send a new message to the session, and confirm new display events arrive without refreshing.
  - Verify: confirm the payload remains close to the underlying file-backed event data.

- [ ] Ensure the backend is the single owner of session runtimes so all clients attached to the same session share one in-process runtime.
  - Verify: open the same session in two browser tabs and confirm both tabs receive the same live updates.
  - Verify: confirm a second attach to the same session does not create a duplicate runtime.

## Milestone 3 — Session actions and permission APIs

- [ ] Implement `POST /api/sessions/:sessionId/messages` to send a new user message.
  - Verify: send a message through the endpoint and confirm it appears in the session `display.jsonl` stream and in the UI.

- [ ] Implement `POST /api/sessions/:sessionId/finish` to finish the current user request.
  - Verify: trigger a long-running request, call `finish`, and confirm the session transitions to a finished/idle state.

- [ ] Implement `POST /api/sessions/:sessionId/cancel` to cancel the current conversation or active run.
  - Verify: start a request, call `cancel`, and confirm the run stops and the session status updates accordingly.

- [ ] Implement permission management endpoints: `GET /api/sessions/:sessionId/permissions`, `POST /api/sessions/:sessionId/permissions/resolve`, `POST /api/sessions/:sessionId/permissions`, and `DELETE /api/sessions/:sessionId/permissions/:permissionId`.
  - Verify: `GET` returns current permission state and any pending requests.
  - Verify: approving or denying a pending request through `resolve` changes the runtime behavior immediately.
  - Verify: adding a permission rule makes the new rule visible in `GET /permissions`.
  - Verify: deleting a permission rule removes it from the returned permission list.

## Milestone 4 — Tool-call and subagent API surface

- [ ] Implement top-level tool-call endpoints: `GET /api/sessions/:sessionId/tool-calls/:toolCallId.jsonl`, `GET /api/sessions/:sessionId/tool-calls/:toolCallId/status.txt`, and `POST /api/sessions/:sessionId/tool-calls/:toolCallId/cancel`.
  - Verify: trigger a tool call from a session and confirm its event stream is available at the public `tool-calls/:toolCallId.jsonl` route.
  - Verify: confirm the backend maps the clean public route to the real internal session files.
  - Verify: cancel the tool call and confirm its status changes and the tool stops.

- [ ] Implement flattened subagent endpoints: `session-meta.json`, `conversation-state.json`, `status.txt`, `token_usage.txt`, and `display.jsonl` under `/api/sessions/:sessionId/subagents/:subagentId/...`.
  - Verify: trigger a subagent, open each flattened route, and confirm the child subagent is reachable without recursive URL nesting.
  - Verify: confirm the returned data identifies parent context in data rather than in the route structure.

- [ ] Implement subagent tool-call endpoints under `/api/sessions/:sessionId/subagents/:subagentId/tool-calls/:toolCallId...`.
  - Verify: trigger a tool call inside a subagent and confirm its status and event stream are reachable from the subagent-scoped routes.
  - Verify: cancel the subagent-owned tool call and confirm it stops.

- [ ] Add coverage for nested subagent addressing so deeply nested subagents still use the flattened session-scoped namespace.
  - Verify: create a nested subagent chain and confirm each subagent can be fetched with `/api/sessions/<sessionId>/subagents/<subagentId>/...`.
  - Verify: confirm recursive routes like `/subagents/<parent>/subagents/<child>/...` are not required.

## Milestone 5 — Frontend shell, auth, and session navigation

- [ ] Scaffold the frontend with Lit, Vite, TypeScript, and plain CSS tokens as described in the PoC.
  - Verify: run the frontend in development mode and confirm the app renders without depending on htmx, Alpine, or Tailwind.
  - Verify: confirm shared colors/spacing are defined with CSS custom properties.

- [ ] Build a login screen that authenticates with `POST /api/auth/login` and restores auth state via `GET /api/auth/session`.
  - Verify: a logged-out user sees the login screen.
  - Verify: after entering the correct secret, the app transitions into the authenticated shell without manual page edits.
  - Verify: reloading the page keeps the user signed in until logout.

- [ ] Build the main app layout with a left sidebar and right main display.
  - Verify: on desktop, the sidebar and main panel appear side by side.
  - Verify: on a narrow/mobile viewport, the layout remains usable for basic chat tasks.

- [ ] Build the sidebar session list and “new conversation” action using `GET /api/sessions` and `POST /api/sessions`.
  - Verify: existing sessions appear in the sidebar.
  - Verify: clicking a session opens it in the main panel.
  - Verify: creating a new conversation adds a new session to the sidebar and opens it.

## Milestone 6 — Conversation UX, detail pages, and approvals

- [ ] Render the main conversation view from `display.jsonl` plus the session snapshot endpoints.
  - Verify: sending a message updates the conversation view live without a page refresh.
  - Verify: status/usage displays refresh correctly as the run progresses.

- [ ] Show tool-call and subagent entries in the main conversation as brief overview cards/rows that can be opened for more detail.
  - Verify: when a tool call or subagent event appears in the main stream, the UI shows a compact summary instead of raw JSON.
  - Verify: clicking the summary opens the matching detail page.

- [ ] Build the detailed tool-call page that displays the full tool-call event stream and current status.
  - Verify: open a tool call from the conversation view and confirm the page shows its history and live updates.
  - Verify: if cancellation is supported in the UI, confirm cancelling from the page updates the status.

- [ ] Build the detailed subagent page that renders a subagent conversation similarly to the main conversation view.
  - Verify: open a subagent from the main conversation and confirm its message history and live updates are visible.
  - Verify: nested subagent content remains reachable through the flattened routes.

- [ ] Add a permission approval popup for pending permission requests, without implementing write/edit diff preview for this PoC.
  - Verify: trigger a permission request and confirm a popup appears with enough information to approve or deny it.
  - Verify: approving or denying from the popup immediately changes the session state.
  - Verify: confirm no diff preview UI is required for this ticket.

## Milestone 7 — End-to-end polish and release readiness

- [ ] Add backend and frontend smoke tests covering login, session creation, message flow, SSE updates, and permission resolution.
  - Verify: run the smoke/integration test suite and confirm the full authenticated happy path passes.

- [ ] Add a short runbook for local use and remote exposure guidance, including the requirement to use HTTPS or a trusted tunnel/proxy when exposed beyond localhost.
  - Verify: follow the runbook on a clean setup and confirm a user can log in, create a session, send a message, and view live updates.

- [ ] Do a final manual PoC pass on desktop and mobile-sized viewports.
  - Verify: on both viewport sizes, confirm the user can log in, switch sessions, create a new conversation, send a message, inspect a tool call, inspect a subagent, and resolve a permission request.
