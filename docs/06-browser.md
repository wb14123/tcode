# Browser Setup

## Why a Browser?

tcode uses headless Chrome for its `web_search` and `web_fetch` tools. Unlike API-based search services, this means the browser uses **your own accounts and logins** — you get your Kagi results, access to sites behind your logins, and the same browsing context you would have in a normal browser.

A shared `browser-server` process manages the Chrome instance, tab pooling, and lifecycle. Multiple tcode sessions share the same browser-server.

## Chrome/Chromium Setup

Install Chrome or Chromium.

**Ubuntu/Debian:**

```bash
sudo apt install chromium-browser
```

Or install Google Chrome from <https://www.google.com/chrome/>.

**macOS:**

```bash
brew install --cask google-chrome
```

Or download from the website.

## First-Time Browser Setup

Run `tcode browser` to launch Chrome with a persistent profile at `~/.tcode/chrome/`. Use this window to log in to your accounts:

- Log in to **Kagi** to use it as your search engine
- Log in to **GitHub**, **Google**, or any other service you want the agent to access
- Google search works without authentication

Cookies, sessions, and all browser storage are saved in the persistent profile and reused by the headless browser-server. You only need to log in once — your sessions carry over across tcode restarts.

This is a standalone command — it opens a visible Chrome window and does not interact with the browser-server process. Close the browser when done.

## Browser Server Configuration

By default, tcode auto-manages a local browser-server via Unix socket at `~/.tcode/browser-server.sock`. Multiple tcode sessions share one server, and it exits after 5 minutes of inactivity.

For a remote browser-server, set in your config file:

```toml
browser_server_url = "http://host:8090"
browser_server_token = "your-bearer-token"
```

See [02-configuration.md](02-configuration.md#browser-server-config) for more details.
