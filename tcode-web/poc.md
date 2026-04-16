# PoC for tcode web

## Goal

Provide a web interface for tcode. Make it easier to use on mobile phones for causal tasks.

Security is a top priority.

## Design 

It have 3 parts:

1. tcode server: like what the existing tcode server do: listen on Conversation events and write them to display files under session dir. Just call existing code.
2. tcode web-server: the backend of tcode web.
  a. It provides APIs and stream the jsonl events from the session files. For example, `/sessions/:id/display.jsonl` stream the `display.jsonl` file in the session dir.
  b. commands like send messages and so on, the same as what `tcode server` accepts
  c. In addition to that, it provides APIs to list existing sessions, and be able to create a new session.
3. tcode frontend: the frontend to listen on web-server events, render it to user, and call tcode web-server for things like sending a new message.

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

`tcode -p <profile> remote --port <port> --password <password>` starts a new remote server

## Security

The server should always be protected by a token/password. Do not need to have user management for now.
