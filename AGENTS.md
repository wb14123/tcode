# LLM Agent Instruction

## Cargo Commands

### Token-Saving Mode (Default)
Always tail cargo output: `cargo check 2>&1 | tail -n 30`, `cargo build 2>&1 | tail -n 30`, `cargo test 2>&1 | tail -n 50`. Re-run without tail if more context needed.

### No Release Mode for Dev
Never `cargo build --release` for dev — too slow. Use `cargo check` (fastest), `cargo build` (debug), `--release` only when user requests.

### After Every Change
Run `cargo fmt` then `cargo clippy --all-targets 2>&1 | tail -n 30` (the `--all-targets` flag also lints tests and examples). Fix all warnings before done.

### Lint Resolution
- **Never add `#[allow(...)]`** to suppress warnings — fix the underlying issue instead.
- Remove unused code, update deprecated APIs, follow naming conventions, etc.
- If a lint genuinely cannot be fixed, discuss with the user before suppressing it.

## Test Organization

- Tests in separate `*_tests.rs` files, one per module (e.g. `conversation_tests.rs`)
- Register in `lib.rs` with `#[cfg(test)] mod <name>_tests;`
- No inline tests in source files

## Test Filesystem Paths

- **Never** write to `/tmp` or use `std::env::temp_dir()` in tests. This pollutes the system temp dir and can leak across runs.
- Always write under the workspace target dir: `env!("CARGO_MANIFEST_DIR")/../target/test-tmp/<module>/<uuid>`.
- **Cleanup before:** remove any stale directory at the test's own `<uuid>` path before creating it (a no-op for a fresh uuid). Never delete the whole `<module>` root inside a test — parallel tests share it.
- **Cleanup after:** own an RAII guard (a `Drop` impl calling `remove_dir_all`) so the per-test directory is removed on both success and panic. Never leave fixture files behind after a test completes.
- Standard pattern (reused across test files):
  ```rust
  /// Per-test temp dir under the workspace target dir; removed on drop
  /// (cleanup runs on success and on panic).
  struct TestDir(std::path::PathBuf);

  impl TestDir {
      fn new(module: &str) -> Self {
          let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
              .join(format!("../target/test-tmp/{module}"));
          std::fs::create_dir_all(&root).expect("failed to create test root");
          let dir = root.join(uuid::Uuid::new_v4().to_string());
          // Cleanup before: remove any stale leftover at this exact path.
          let _ = std::fs::remove_dir_all(&dir);
          std::fs::create_dir_all(&dir).expect("failed to create test dir");
          Self(dir)
      }

      fn path(&self) -> &std::path::Path {
          &self.0
      }
  }

  impl Drop for TestDir {
      fn drop(&mut self) {
          let _ = std::fs::remove_dir_all(&self.0);
      }
  }
  ```
- `target/` is already gitignored and cleaned by `cargo clean`, so leftovers from a crashed/interrupted run are harmless and self-cleaning.
- See `tools/src/file_permission_tests.rs`, `tools/src/grep_tool/grep_tool_tests.rs`, `llm-rs/src/skill_tests.rs` for reference implementations.

## Error Handling

- Never `let _ =` to discard `Result` — at minimum log with `tracing::error!`/`tracing::warn!`
- Prefer returning `Result` to caller
- Never `.unwrap()` in production — prefer `?`, `if let`/`match`/`let...else`, or `.unwrap_or()`/`.unwrap_or_default()`
- `expect("reason")` only for truly infallible cases (hardcoded parses, values verified on preceding line)
- Tests: prefer `-> anyhow::Result<()>` with `?` over `.unwrap()`
- Uses `parking_lot::Mutex`/`RwLock` (not `std::sync`) — no `.lock().unwrap()` needed

## No Lossy UTF-8 Conversions

- **Never** use `String::from_utf8_lossy`, `Path::to_string_lossy`, or any other lossy UTF-8 conversion (e.g. per-chunk `TextDecoder` in JS).
- Lossy decoding silently replaces invalid bytes with U+FFFD. In streaming/chunked data it is worse: a multi-byte character split across two chunks gets corrupted on each side and the raw bytes are lost forever (see issue #2).
- The LLM pipeline is UTF-8-only (JSON requests/responses), so non-UTF-8 data can only come from tools (files, command output). Fail loudly with a clear error instead of mangling.
- Correct patterns:
  - Complete buffers: `std::str::from_utf8(...)` / `String::from_utf8(...)`, propagate the error with context (byte offset, line number, file path).
  - Streaming/chunked data: accumulate raw bytes in `Vec<u8>` and decode only complete `\n`-terminated lines (never decode per chunk). A `\n`-delimited line always contains complete UTF-8 characters. Reference: `tools/src/read/mod.rs`, `tcode-web/src/routes/api.rs`.
  - On invalid UTF-8 in tool output: emit an explicit error-styled message telling the LLM the content was omitted and how to inspect it (e.g. re-run piped through `base64`), plus `tracing::warn!`.
  - Paths: `path.to_str()` and propagate an error. Use `{:?}` (Debug escaping, lossless) for log-only display. `.to_str().unwrap()` is allowed only in tests (test-created paths are always UTF-8).
- Never convert non-UTF-8 bytes to U+FFFD to "make them work" - fix the decode boundary instead.

## Discussion Before Code Changes

- **Always discuss** design/heuristic/behavioral questions with user before writing code
- Do NOT speculatively implement during a discussion — wait for confirmation
