# Fix: Empty "► INFO" Lines in Display Window

## Bug Description

Empty `► INFO` text sometimes appears as raw visible text in the display window. This is a display regression introduced by commit `fa74964` ("use tree sitter to improve highlight").

## Root Cause

Commit `fa74964` changed the display buffer architecture: separator lines went from invisible empty strings (`''`) to visible marker text (`'► INFO'`, `'► END'`, etc.) that get concealed by Neovim overlay virtual text (`virt_text_pos = 'overlay'`).

The bug triggers on **every successful tool call**:

1. `ToolMessageEnd` always has `input_tokens: 0, output_tokens: 0` — by design, since tools don't call LLMs and have no token counts. Every single Rust emission site hardcodes these to `0`. This has been the case since the very first commit that introduced `ToolMessageEnd`.

2. The `render_info` function in `tcode/lua/tcode.lua` correctly skips displaying `[TOOL: 0 in / 0 out tokens]` via the `has_tokens` guard:
   ```lua
   local has_tokens = not token_prefix or (data.input_tokens > 0 or data.output_tokens > 0)
   ```
   When `token_prefix` is `'TOOL'` and both token counts are `0`, `has_tokens` is `false`, so no token text is added to `virt_parts`.

3. The tool's `end_status` is `'Succeeded'`, so the error-status branch is also skipped.

4. Result: `virt_parts` is empty `{}`. The fallback overlay is `{ { '', '' } }` (zero-width string), which conceals **zero** buffer characters. The underlying `► INFO` buffer text remains fully visible.

Before this commit, the buffer text was `''` (empty), so an empty overlay was harmless — just a blank line.

### Affected call sites

There are exactly two call sites for `render_info`:

1. **`AssistantMessageEnd` (line ~635)**: Called with `render_info(buf, ns, data, nil)`. Token fields are always integers (even `0` is truthy in Lua), and `token_prefix` is `nil` so `not token_prefix` is `true` making `has_tokens` always `true`. **This call site always produces non-empty `virt_parts` — NOT affected.**

2. **`ToolMessageEnd` (line ~812)**: Called with `render_info(buf, ns, data, 'TOOL', insert_row)`. Tokens are always `0/0`, `token_prefix` is `'TOOL'`, and successful tools have `end_status = 'Succeeded'`. **This call site produces empty `virt_parts` on every successful tool call — THIS IS THE BUG.**

### Note on `► END` (dead code, not a user-visible bug)

The `AssistantRequestEnd` handler (line ~1139) has the same pattern: it writes `► END` and falls back to `{ { '', '' } }` when `data.total_input_tokens` is nil. However, the Rust side always sends integer values for these fields (never omitted/null), and `0` is truthy in Lua, so the `else` branch is dead code. Not a user-visible issue, but could be cleaned up.

## Fix Plan

### Step 1: Refactor `render_info()` in `tcode/lua/tcode.lua`

Currently `render_info` writes the `► INFO` buffer line first, then computes `virt_parts`, then sets the overlay. The fix is to **compute `virt_parts` first, and skip writing the buffer line + extmark entirely if `virt_parts` is empty**.

Key considerations:
- The function has two modes: append (when `insert_row` is nil) and insert (when `insert_row` is provided, used by `ToolMessageEnd`). Both modes need the same early-return logic.
- When skipping, nothing should be written to the buffer — no `► INFO` line, no extmark. This restores the pre-commit behavior for successful tool calls.
- When NOT skipping (assistant messages with tokens, or failed tool calls), behavior stays exactly the same: write `► INFO`, set overlay with `virt_parts`.

### Step 2: Remove `► END` dead code in `AssistantRequestEnd` handler

The `AssistantRequestEnd` handler (line ~1139) has an `else` branch that writes `► END` with an empty overlay `{ { '', '' } }` when `data.total_input_tokens` is nil. This branch is dead code: the Rust side always sends integer values for these fields, and `0` is truthy in Lua. Remove the `if`/`else` conditional and keep only the body of the `if` branch (the one that builds the real token overlay).

### Step 3: Verify

- Run with tool calls (file reads, bash commands, web searches, etc.) and confirm no empty `► INFO` lines appear.
- Confirm that assistant message `► INFO` lines (which have real token counts) still display correctly with the token overlay.
- Confirm that failed tool calls still show their error status (e.g., `[TOOL Failed]`).

### Why not alternatives?

- **Making the overlay text non-empty (e.g., spaces matching the width)**: Fragile — depends on exact character width of `► INFO` (the `►` glyph width varies by font), and still leaves a visually empty line taking up vertical space for no reason.
- **Removing token fields from `ToolMessageEnd` in Rust**: Larger refactor across Rust struct definitions and all emission sites for no user-visible benefit; the Lua side already handles zeros correctly.
- **Always showing `[TOOL: 0 in / 0 out tokens]`**: Noisy and misleading — these aren't real token counts.
