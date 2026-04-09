//! Interactive wizard for creating a new tcode config file.
//!
//! Prompts the user for `provider`, `base_url`, and `api_key`, then writes
//! a config file based on [`crate::config::DEFAULT_CONFIG_TEMPLATE`]. All
//! other options remain as commented-out hints in the generated file.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use dialoguer::{Input, Select, theme::ColorfulTheme};

/// Default base URL for each supported provider string. Mirrors the values
/// in `main.rs::Provider::default_base_url` so the wizard stays decoupled
/// from the `Provider` enum (which is private to `main.rs`).
fn default_base_url_for(provider: &str) -> &'static str {
    match provider {
        "claude" => "https://api.anthropic.com",
        "open-ai" => "https://api.openai.com/v1",
        "open-router" => "https://openrouter.ai/api/v1",
        other => unreachable!(
            "default_base_url_for called with unknown provider {other:?}; \
             WizardChoice::provider_str must return one of the three known values"
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

/// Identifier for the wizard's provider choice. Both `Claude` and
/// `ClaudeOauth` write `provider = "claude"` to the config file — the
/// `ClaudeOauth` variant just skips the API-key prompt and hints at
/// `tcode claude-auth`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WizardChoice {
    Claude,
    ClaudeOauth,
    OpenAi,
    OpenRouter,
}

impl WizardChoice {
    fn provider_str(&self) -> &'static str {
        match self {
            WizardChoice::Claude | WizardChoice::ClaudeOauth => "claude",
            WizardChoice::OpenAi => "open-ai",
            WizardChoice::OpenRouter => "open-router",
        }
    }

    fn env_var_name(&self) -> &'static str {
        match self {
            WizardChoice::Claude | WizardChoice::ClaudeOauth => "ANTHROPIC_API_KEY",
            WizardChoice::OpenAi => "OPENAI_API_KEY",
            WizardChoice::OpenRouter => "OPENROUTER_API_KEY",
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

    let theme = ColorfulTheme::default();

    // --- Provider select --------------------------------------------------
    let items = [
        "claude           — Anthropic API key",
        "claude-oauth     — Claude Pro/Max subscription (OAuth)",
        "open-ai          — OpenAI API key",
        "open-router      — OpenRouter API key",
    ];
    let idx = Select::with_theme(&theme)
        .with_prompt("Select a provider")
        .items(&items)
        .interact()
        .context("provider selection failed")?;
    let choice = match idx {
        0 => WizardChoice::Claude,
        1 => WizardChoice::ClaudeOauth,
        2 => WizardChoice::OpenAi,
        3 => WizardChoice::OpenRouter,
        other => bail!("unexpected provider selection index: {other}"),
    };
    let provider_str = choice.provider_str();

    // --- Base URL input ---------------------------------------------------
    let default_base_url = default_base_url_for(provider_str);
    let base_url_input: String = Input::with_theme(&theme)
        .with_prompt("Base URL")
        .with_initial_text(default_base_url)
        .interact_text()
        .context("base URL input failed")?;

    // --- API key input (skipped for claude-oauth) ------------------------
    let api_key_input: Option<String> = if choice == WizardChoice::ClaudeOauth {
        None
    } else {
        let prompt = format!("API key (leave empty to use ${})", choice.env_var_name());
        let raw: String = Input::with_theme(&theme)
            .with_prompt(prompt)
            .allow_empty(true)
            .interact_text()
            .context("API key input failed")?;
        Some(raw)
    };

    // --- Compute overrides ------------------------------------------------
    let base_url_trimmed = base_url_input.trim();
    validate_no_control_chars("base URL", base_url_trimmed)?;
    let base_url_override: Option<&str> =
        if base_url_trimmed.is_empty() || base_url_trimmed == default_base_url.trim() {
            None
        } else {
            Some(base_url_trimmed)
        };

    let api_key_trimmed = api_key_input.as_ref().map(|s| s.trim());
    if let Some(k) = api_key_trimmed
        && !k.is_empty()
    {
        validate_no_control_chars("API key", k)?;
    }
    let api_key_override: Option<&str> = match api_key_trimmed {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    };

    // --- Render file contents ---------------------------------------------
    let contents = substitute_template(
        crate::config::DEFAULT_CONFIG_TEMPLATE,
        provider_str,
        base_url_override,
        api_key_override,
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
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            println!();
            println!("WARNING: ANTHROPIC_API_KEY is currently set in your environment.");
            println!("tcode will prefer the env var over your OAuth tokens. Unset it");
            println!("(e.g. `unset ANTHROPIC_API_KEY`) before running tcode if you");
            println!("want to use your Claude Pro/Max subscription credits.");
        }
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
