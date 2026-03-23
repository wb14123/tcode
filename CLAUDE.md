# Claude Code Project Instructions

## Cargo Commands

### Token-Saving Mode (Default)
When running cargo commands for routine checks, always tail the output to save tokens:

```bash
cargo check 2>&1 | tail -n 30
cargo build 2>&1 | tail -n 30
cargo test 2>&1 | tail -n 50
```

If compilation fails and more context is needed, re-run without tail to see full output.

### Never Use Release Mode for Development
- **Never** run `cargo build --release` for checking or development builds - it's extremely slow
- Use `cargo build` (debug mode) for development
- Use `cargo check` for quick syntax/type checking (fastest)
- Only use `--release` when explicitly requested by the user for production builds

### Code Quality (Required After Every Change)
After every code change, run:
```bash
cargo fmt
cargo clippy 2>&1 | tail -n 30
```
Fix any warnings or formatting issues before considering the task complete.

### Command Priority
1. `cargo check` - Use for quick validation (fastest)
2. `cargo build` - Use when actually need to run the binary
3. `cargo test` - Use for running tests

## Test Organization

- Place tests in separate `*_tests.rs` files, one per module being tested (e.g. `conversation_tests.rs`, `llm_tests.rs`, `tool_tests.rs`)
- Register test modules in `lib.rs` with `#[cfg(test)] mod <name>_tests;`
- Do NOT put tests inline in source files — keep source and test files separate

## Error Handling

- **Never** use `let _ =` to silently discard `Result` values
- At minimum, log the error with `tracing::error!` or `tracing::warn!`
- Prefer returning `Result` to the caller so they can decide how to handle the error
- This applies to both production code and fire-and-forget calls (e.g. broadcasting, file writes)

### Avoid `unwrap()` and `expect()`

- **Never** use `.unwrap()` in production code
- **Prefer `?`** whenever the function can return `Result` or `Option`
- Use `if let` / `match` / `let...else` when handling errors locally
- Use `.unwrap_or()` / `.unwrap_or_default()` when a fallback value makes sense
- **`expect("reason")`** is acceptable only for truly infallible cases (e.g. hardcoded string parses, `SystemTime::duration_since(UNIX_EPOCH)`, values verified on the preceding line)
- In tests, prefer `-> anyhow::Result<()>` with `?` over `.unwrap()`
- This codebase uses `parking_lot::Mutex` and `parking_lot::RwLock` (not `std::sync`) to avoid lock poisoning and the `.lock().unwrap()` pattern

## Discussion Before Code Changes

- When discussing design, heuristics, or behavioral questions — **always talk to the user first** before writing code
- Do NOT speculatively implement a fix during a discussion — wait for the user to confirm the approach
- This is especially important for heuristic-based logic where multiple approaches exist and none is obviously correct
