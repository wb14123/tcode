# Browser Setup

## Why a Browser?

tcode uses headless Chrome for its `web_search` and `web_fetch` tools. Unlike API-based search services, this means the browser uses **your own accounts and logins** — you get your Kagi results, access to sites behind your logins, and the same browsing context you would have in a normal browser. Web-only sessions rely on this same browser setup.

A shared `browser-server` process manages the Chrome instance, tab pooling, and lifecycle. Multiple tcode sessions share the same browser-server.

## Chrome/Chromium Setup

Install Chrome or Chromium.

**Ubuntu/Debian:**

On Ubuntu 24.04 and later, `apt install chromium-browser` redirects to the
snap package, which works with tcode. The browser profile path differs for
snap packages (see [Profile location](#profile-location) below).

```bash
sudo apt install chromium-browser
```

To install a non-snap Chromium via PPA:

```bash
sudo add-apt-repository ppa:xtradeb/apps -y
sudo apt update
sudo apt install chromium
```

Alternatively, install Google Chrome from <https://www.google.com/chrome/>.

**macOS:**

```bash
brew install --cask google-chrome
```

Or download from the website.

## First-Time Browser Setup

Run `tcode browser` to launch Chrome with a persistent profile. Use this
window to log in to your accounts:

- Log in to **Kagi** to use it as your search engine
- Log in to **GitHub**, **Google**, or any other service you want the agent
  to access
- Google search technically works without authentication, but a fresh
  profile will usually hit Google's CAPTCHA / "unusual traffic" page on
  the first queries and searches will fail. Before using `web_search`
  against Google, either log in to your Google account in this window, or
  run a few manual Google searches here to warm the profile up. Cookies
  persist and are reused by the headless browser-server.

### Profile location

tcode automatically selects the profile directory based on your Chrome
installation:

| Chrome package        | Profile path                                  |
|-----------------------|-----------------------------------------------|
| system (deb, PPA)    | `~/.tcode/chrome/`                            |
| Google Chrome (deb)  | `~/.tcode/chrome/`                            |
| Snap (chromium)      | `~/snap/chromium/common/.tcode-chrome/`       |
| Snap (google-chrome) | `~/snap/google-chrome/common/.tcode-chrome/`  |

Cookies, sessions, and all browser storage are saved in this persistent
profile and reused by the headless browser-server. You only need to log
in once — your sessions carry over across tcode restarts. Run this setup
before using web-only sessions if you need logged-in search or page
access.

This is a standalone command — it opens a visible Chrome window and does not interact with the browser-server process. When finished, press Ctrl+C in the terminal to exit (closing the Chrome window first is optional).

## Browser Server Configuration

By default, tcode auto-manages a local browser-server via Unix socket at `~/.tcode/browser-server.sock`. Multiple tcode sessions share one server, and it exits after 5 minutes of inactivity.

For a remote browser-server, set in your config file:

```toml
browser_server_url = "http://host:8090"
browser_server_token = "your-bearer-token"
```

See [02-configuration.md](02-configuration.md#browser-server-config) for more details.
