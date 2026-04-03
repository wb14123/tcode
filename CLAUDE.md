# LLM Agent Instruction

## Cargo Commands

### Token-Saving Mode (Default)
Always tail cargo output: `cargo check 2>&1 | tail -n 30`, `cargo build 2>&1 | tail -n 30`, `cargo test 2>&1 | tail -n 50`. Re-run without tail if more context needed.

### No Release Mode for Dev
Never `cargo build --release` for dev — too slow. Use `cargo check` (fastest), `cargo build` (debug), `--release` only when user requests.

### After Every Change
Run `cargo fmt` then `cargo clippy 2>&1 | tail -n 30`. Fix all warnings before done.

## Test Organization

- Tests in separate `*_tests.rs` files, one per module (e.g. `conversation_tests.rs`)
- Register in `lib.rs` with `#[cfg(test)] mod <name>_tests;`
- No inline tests in source files

## Error Handling

- Never `let _ =` to discard `Result` — at minimum log with `tracing::error!`/`tracing::warn!`
- Prefer returning `Result` to caller
- Never `.unwrap()` in production — prefer `?`, `if let`/`match`/`let...else`, or `.unwrap_or()`/`.unwrap_or_default()`
- `expect("reason")` only for truly infallible cases (hardcoded parses, values verified on preceding line)
- Tests: prefer `-> anyhow::Result<()>` with `?` over `.unwrap()`
- Uses `parking_lot::Mutex`/`RwLock` (not `std::sync`) — no `.lock().unwrap()` needed

## Discussion Before Code Changes

- **Always discuss** design/heuristic/behavioral questions with user before writing code
- Do NOT speculatively implement during a discussion — wait for confirmation
