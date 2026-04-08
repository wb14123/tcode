# Neovim Setup Guide

## Overview

tcode opens neovim instances for the display and edit panes. The neovim plugin (`tcode.lua`) is embedded in the tcode binary and auto-installed to neovim's runtime path at startup -- no manual plugin installation is needed.

## LazyVim Setup

LazyVim is the recommended neovim distribution. No tcode-specific LazyVim configuration is required -- tcode manages its own neovim instances with the embedded plugin. However, plugins you have installed in LazyVim (like render-markdown.nvim) will be available in tcode's neovim windows too.

## render-markdown.nvim

tcode uses its own filetype (`tcode`) for the display buffer. render-markdown.nvim needs to be told about this filetype so it renders content correctly. Add this LazyVim plugin spec:

```lua
return {
  {
    "MeanderingProgrammer/render-markdown.nvim",
    ft = { "markdown", "tcode" },
    opts = {
      file_types = { "markdown", "tcode" },
    },
  },
}
```

This enables rendered headings, code blocks, lists, and other markdown elements inside the display pane.

## Tree-Sitter Syntax Highlighting

tcode includes a custom tree-sitter grammar (`tree-sitter-tcode`) that parses the display buffer:

- The grammar splits content at separator lines (the `►` delimiters between messages) and injects each content region as **markdown**.
- This means neovim's built-in markdown tree-sitter highlighting works inside tcode buffers.
- The external scanner (`scanner.c`) recognizes `►` lines as separators and content between them as content blocks.
- `highlights.scm` styles the separator lines.
- `injections.scm` tells neovim to parse each content block as markdown.
- At runtime, tcode loads the parser via `vim.treesitter.language.add()` and writes query files into the session directory.

**Installation:** `install.sh` compiles the shared library (`libtree-sitter-tcode.so` / `.dylib`) via `make` and copies it to `/usr/lib`. The parser is then found automatically by the tcode binary at startup.

## Plugin Compatibility Notes

tcode windows use custom statuslines to show connection status, token usage, and keybinding hints. On setup, tcode automatically disables conflicting plugins in its neovim instances:

- **Statusline plugins** (lualine, vim-airline, lightline) -- disabled and their autocmds removed so they cannot re-assert.
- **Dashboard/start screen plugins** (alpha-nvim, dashboard-nvim, snacks.nvim dashboard, mini.starter) -- their buffers are wiped on setup.

This only affects tcode's own neovim processes -- your other neovim instances are not affected.

## Shortcut Templates

The edit view supports `/shortcut` + Tab expansion for prompt templates. See [02-configuration.md](02-configuration.md#shortcut-templates) for setup and usage details.
