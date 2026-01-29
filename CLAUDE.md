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

### Command Priority
1. `cargo check` - Use for quick validation (fastest)
2. `cargo build` - Use when actually need to run the binary
3. `cargo test` - Use for running tests
