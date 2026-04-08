# tree-sitter-tcode

A [tree-sitter](https://tree-sitter.github.io/) grammar for the tcode display format. The compiled shared library (`libtree-sitter-tcode.so` / `.dylib`) is loaded by Neovim at runtime to provide syntax highlighting and markdown injection in tcode conversation buffers.

## Format

A tcode buffer is a sequence of **blocks**, each starting with a **separator line** -- a line beginning with `►` (U+25BA) at column 0, followed by a role label:

```
► USER
What is 2 + 2?
► ASSISTANT
The answer is **4**.
► TOOL
{...tool output...}
► END
```

The grammar splits this into `separator` and `content` nodes. Everything between two separator lines is a single `content` node.

## Grammar

The grammar itself (`grammar.js`) is minimal -- just two externally-scanned token types:

```js
rules: {
    document: $ => repeat($.block),
    block: $ => seq($.separator, optional($.content)),
}
```

All real tokenization happens in the external scanner.

## External scanner

`src/scanner.c` implements the two token types:

- **SEPARATOR**: Matches a full line starting with `►` at column 0. Consumes through the newline.
- **CONTENT**: Consumes all characters after a separator until EOF or the start of the next `►`-prefixed line. Stops *before* the next separator so it becomes part of the next block.

The scanner is stateless (no serialization needed).

## Query files

### `queries/highlights.scm`

Highlights separator lines as `@comment`, which dims them relative to content. tcode's Lua layer replaces them with virtual text (extmarks), so the tree-sitter highlight mainly serves as a fallback.

### `queries/injections.scm`

Injects **markdown** parsing into every `content` node. This gives content blocks full markdown highlighting (headings, bold, code blocks, etc.) via Neovim's built-in markdown tree-sitter parser.

## Runtime integration

At startup, `tcode` (the Rust binary):

1. `include_str!`s both `.scm` query files and writes them to a session-scoped cache directory under `queries/tcode/`.
2. Locates the compiled `.so`/`.dylib` (next to the executable, or in `../lib/`).
3. Passes the parser path to Neovim, which loads it via `vim.treesitter.language.add('tcode', { path = ... })`.

Neovim then discovers the query files via its standard `runtimepath` mechanism (the session cache dir is added to `rtp`).

## Building

Requires a C compiler. The generated parser source is checked in, so `tree-sitter` CLI is only needed after editing `grammar.js`.

```sh
# Build the shared library
make

# Regenerate parser from grammar.js (requires tree-sitter CLI)
make generate

# Clean build artifacts
make clean
```

The output is `libtree-sitter-tcode.so` (Linux) or `libtree-sitter-tcode.dylib` (macOS). The project Makefile copies this next to the `tcode` binary during the full build.
