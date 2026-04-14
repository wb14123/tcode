//! Interactive wizard for creating a new tcode config file.
//!
//! Prompts the user for `provider`, `base_url`, and `api_key`, then writes
//! a config file based on [`crate::config::DEFAULT_CONFIG_TEMPLATE`]. All
//! other options remain as commented-out hints in the generated file.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rustyline::Editor;
use rustyline::error::ReadlineError;
use rustyline::history::MemHistory;

/// Default base URL for each supported provider string. Mirrors the values
/// in `main.rs::Provider::default_base_url` so the wizard stays decoupled
/// from the `Provider` enum (which is private to `main.rs`).
///
/// Only called for the three API-key providers: `ClaudeOauth` skips the
/// base URL prompt entirely, so `"claude-oauth"` is never passed here.
fn default_base_url_for(provider: &str) -> &'static str {
    match provider {
        "claude" => "https://api.anthropic.com",
        "open-ai" => "https://api.openai.com/v1",
        "open-router" => "https://openrouter.ai/api/v1",
        other => unreachable!(
            "default_base_url_for called with unknown provider {other:?}; \
             the wizard only calls this for claude, open-ai, or open-router"
        ),
    }
}

/// Reject values that would break the single-line shape of the TOML template.
/// `substitute_template` assumes each rewritten value fits on one line, but
/// `toml::Value::String(...).to_string()` will emit a multi-line `"""..."""`
/// literal if the value contains `\n`/`\r`, which corrupts the one-line
/// invariant of `try_rewrite`.
fn validate_no_control_chars(field: &str, value: &str) -> anyhow::Result<()> {
    if let Some(bad) = value.chars().find(|c| c.is_control()) {
        anyhow::bail!(
            "{field} contains a control character (U+{:04X}); please retype without newlines or tabs",
            bad as u32
        );
    }
    Ok(())
}

/// Identifier for the wizard's provider choice. `ClaudeOauth` writes
/// `provider = "claude-oauth"` to the config file and skips both the
/// base URL and API-key prompts; at runtime tcode loads OAuth tokens via
/// `tcode claude-auth` and ignores `api_key` / `$ANTHROPIC_API_KEY`.
/// `OpenAiOauth` is analogous, writing `provider = "open-ai-oauth"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WizardChoice {
    Claude,
    ClaudeOauth,
    OpenAi,
    OpenAiOauth,
    OpenRouter,
}

impl WizardChoice {
    fn provider_str(&self) -> &'static str {
        match self {
            WizardChoice::Claude => "claude",
            WizardChoice::ClaudeOauth => "claude-oauth",
            WizardChoice::OpenAi => "open-ai",
            WizardChoice::OpenAiOauth => "open-ai-oauth",
            WizardChoice::OpenRouter => "open-router",
        }
    }

    /// Environment variable name for the provider's API key.
    ///
    /// Not defined for `ClaudeOauth` or `OpenAiOauth`: the OAuth flows
    /// skip the API-key prompt entirely, so this function is structurally
    /// unreachable for those variants.
    fn env_var_name(&self) -> &'static str {
        match self {
            WizardChoice::Claude => "ANTHROPIC_API_KEY",
            WizardChoice::OpenAi => "OPENAI_API_KEY",
            WizardChoice::OpenRouter => "OPENROUTER_API_KEY",
            WizardChoice::ClaudeOauth | WizardChoice::OpenAiOauth => {
                unreachable!("env_var_name called on an OAuth WizardChoice variant")
            }
        }
    }
}

/// Run the interactive setup wizard.
///
/// If `first_run` is `true`, an additional "Run `tcode` again to start"
/// message is printed at the end — used by the bare `tcode` auto-launch
/// path so the user knows to re-invoke the binary.
pub fn run(profile: Option<&str>, first_run: bool) -> Result<()> {
    let target = crate::config::config_path_for(profile)?;

    if target.exists() {
        bail!(
            "Config already exists at {}.\nEdit it directly, or delete it first and re-run `tcode config`.",
            target.display()
        );
    }

    // One rustyline Editor reused across all prompts. `MemHistory` is pinned
    // explicitly (rather than via `DefaultEditor`) so the history backend
    // stays in-memory regardless of any future Cargo feature edits — the API
    // key must never touch the filesystem via rustyline.
    let mut rl: Editor<(), MemHistory> =
        Editor::new().context("failed to initialize line editor")?;

    // --- Provider select --------------------------------------------------
    println!("Select a provider:");
    println!("  1) claude           — Anthropic API key");
    println!("  2) claude-oauth     — Claude Pro/Max subscription (OAuth)");
    println!("  3) open-ai          — OpenAI API key");
    println!("  4) open-ai-oauth    — OpenAI with Codex subscription (OAuth login)");
    println!("  5) open-router      — OpenRouter API key");
    let choice = loop {
        let line = match rl.readline("Enter a number [1-5]: ") {
            Ok(s) => s,
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                bail!("wizard cancelled");
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context("provider selection failed"));
            }
        };
        match line.trim() {
            "1" => break WizardChoice::Claude,
            "2" => break WizardChoice::ClaudeOauth,
            "3" => break WizardChoice::OpenAi,
            "4" => break WizardChoice::OpenAiOauth,
            "5" => break WizardChoice::OpenRouter,
            other => {
                println!("Invalid choice {other:?}; please enter 1, 2, 3, 4, or 5.");
            }
        }
    };
    let provider_str = choice.provider_str();

    // --- Base URL input (skipped for OAuth providers) ---------------------
    let base_url_override: Option<String> = if choice == WizardChoice::ClaudeOauth
        || choice == WizardChoice::OpenAiOauth
    {
        None
    } else {
        let default_base_url = default_base_url_for(provider_str);
        // `(default_base_url, "")` pre-fills the line and places the cursor
        // at the end, so the user can edit in place with arrow keys and
        // backspace.
        let base_url_input = match rl.readline_with_initial("Base URL: ", (default_base_url, "")) {
            Ok(s) => s,
            Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                bail!("wizard cancelled");
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context("base URL input failed"));
            }
        };
        let trimmed = base_url_input.trim();
        validate_no_control_chars("base URL", trimmed)?;
        if trimmed.is_empty() || trimmed == default_base_url.trim() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    // --- API key input (skipped for OAuth providers) ----------------------
    //
    // For the API-key providers, empty input is a real value: it
    // writes an uncommented `api_key = ""` line to the config file. At
    // runtime, an empty `api_key` in the config falls back to the env var
    // if set, or passes "" through to the LLM client (for self-hosted
    // unauthenticated endpoints).
    let api_key_override: Option<String> =
        if choice == WizardChoice::ClaudeOauth || choice == WizardChoice::OpenAiOauth {
            None
        } else {
            let prompt = format!(
                "API key (empty means no auth or use ${}): ",
                choice.env_var_name()
            );
            let raw = match rl.readline(&prompt) {
                Ok(s) => s,
                Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                    bail!("wizard cancelled");
                }
                Err(e) => {
                    return Err(anyhow::Error::new(e).context("API key input failed"));
                }
            };
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                validate_no_control_chars("API key", trimmed)?;
            }
            Some(trimmed.to_string())
        };

    // --- Render file contents ---------------------------------------------
    let contents = substitute_template(
        crate::config::DEFAULT_CONFIG_TEMPLATE,
        provider_str,
        base_url_override.as_deref(),
        api_key_override.as_deref(),
    );

    // --- Write file atomically --------------------------------------------
    write_config_file(&target, &contents)?;

    // --- Next-steps output ------------------------------------------------
    println!();
    println!("Config written to: {}", target.display());
    println!();
    println!("To edit more options (model, layout, shortcuts, subagent limits,");
    println!("browser server, search engine, etc.), open the file directly.");
    println!("See the configuration reference:");
    println!("  https://github.com/wb14123/tcode/blob/main/docs/02-configuration.md");

    if choice == WizardChoice::ClaudeOauth {
        println!();
        println!("Next: run `tcode claude-auth` to authenticate with your Claude");
        println!("Pro/Max account.");
    }

    if choice == WizardChoice::OpenAiOauth {
        println!();
        println!("Next: run `tcode openai-auth` to authenticate with your OpenAI");
        println!("Codex subscription.");
    }

    if first_run {
        println!();
        println!("Setup complete. Run `tcode` again to start.");
    }

    Ok(())
}

/// Create the parent directory, write `contents` to a sibling temp file with
/// `0600` permissions on Unix, then atomically rename it over `target`.
fn write_config_file(target: &Path, contents: &str) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create parent directory {} for config",
                parent.display()
            )
        })?;
    }

    // Build a sibling temp path: <target>.tmp
    let tmp_path = {
        let mut os = target.as_os_str().to_owned();
        os.push(".tmp");
        std::path::PathBuf::from(os)
    };

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);

    let mut file = match opts.open(&tmp_path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            bail!(
                "stale temp file at {}; remove it and re-run `tcode config`",
                tmp_path.display()
            );
        }
        Err(e) => {
            return Err(anyhow::Error::new(e)
                .context(format!("failed to open temp file {}", tmp_path.display())));
        }
    };

    file.write_all(contents.as_bytes())
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    if let Err(e) = file.sync_all() {
        tracing::warn!("sync_all on {} failed: {e}", tmp_path.display());
    }
    drop(file);

    // The file was created with 0o600 on Unix via OpenOptionsExt::mode above,
    // so there's no window where the API key is world-readable.

    std::fs::rename(&tmp_path, target).with_context(|| {
        format!(
            "failed to rename {} to {}",
            tmp_path.display(),
            target.display()
        )
    })?;

    Ok(())
}

/// Substitute user-provided values into `template` by rewriting the three
/// commented `# provider = "..."`, `# base_url = "..."`, and
/// `# api_key = "..."` lines (if present) to uncommented assignments.
///
/// All other lines are preserved verbatim. Values are TOML-escaped via
/// `toml::Value::String(...)` so quotes and backslashes in user input
/// produce valid TOML.
///
/// Only the first matching commented line is rewritten for each key; any
/// further example lines for the same key are left untouched so the output
/// never contains duplicate key assignments.
///
/// # Precondition
///
/// `provider`, `base_url_override`, and `api_key` values must not contain
/// control characters (including `\n`, `\r`, `\t`). If they do, the toml
/// crate will serialize them as multi-line `"""..."""` literals, which
/// breaks the "one commented line in, one uncommented line out" invariant
/// of the line-based rewrite. Callers (see `run()`) validate this upfront.
pub(crate) fn substitute_template(
    template: &str,
    provider: &str,
    base_url_override: Option<&str>,
    api_key: Option<&str>,
) -> String {
    let ends_with_newline = template.ends_with('\n');
    let mut out: Vec<String> = Vec::new();
    let mut done_provider = false;
    let mut done_base_url = false;
    let mut done_api_key = false;

    for line in template.lines() {
        if !done_provider && let Some(replaced) = try_rewrite(line, "provider", Some(provider)) {
            out.push(replaced);
            done_provider = true;
            continue;
        }
        if !done_base_url && let Some(replaced) = try_rewrite(line, "base_url", base_url_override) {
            out.push(replaced);
            done_base_url = true;
            continue;
        }
        if !done_api_key && let Some(replaced) = try_rewrite(line, "api_key", api_key) {
            out.push(replaced);
            done_api_key = true;
            continue;
        }
        out.push(line.to_string());
    }

    let mut joined = out.join("\n");
    if ends_with_newline {
        joined.push('\n');
    }
    joined
}

/// If `line` is a commented assignment of the form `# <key> = "..."`
/// (possibly with leading whitespace), and `value` is `Some`, return a
/// rewritten uncommented assignment preserving any trailing inline comment
/// after the old quoted value. If `line` doesn't match the key, or `value`
/// is `None`, return `None` to indicate no rewrite.
fn try_rewrite(line: &str, key: &str, value: Option<&str>) -> Option<String> {
    let value = value?;

    // Strip leading whitespace.
    let trimmed = line.trim_start();
    // Must start with `#`.
    let after_hash = trimmed.strip_prefix('#')?;
    // Tolerate any amount of whitespace (or none) between `#` and the key,
    // so `#provider`, `# provider`, `#  provider`, and `#\tprovider` all
    // match.
    let after_hash = after_hash.trim_start();
    // Must start with `<key> = "`.
    let prefix = format!("{key} = \"");
    if !after_hash.starts_with(&prefix) {
        return None;
    }
    // Locate the closing `"` of the old value.
    let after_open_quote = &after_hash[prefix.len()..];
    let close_rel = after_open_quote.find('"')?;
    // `tail` is everything after the closing `"` — this preserves any
    // trailing inline `# ...` comment and surrounding whitespace.
    let tail = &after_open_quote[close_rel + 1..];

    // Escape the user-supplied value via the toml crate so quotes,
    // backslashes, and control characters produce valid TOML.
    let escaped = toml::Value::String(value.to_string()).to_string();

    Some(format!("{key} = {escaped}{tail}"))
}
