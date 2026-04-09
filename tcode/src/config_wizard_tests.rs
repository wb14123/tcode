use crate::config::{DEFAULT_CONFIG_TEMPLATE, TcodeConfig};
use crate::config_wizard::substitute_template;

#[test]
fn test_substitute_all_three_fields() -> anyhow::Result<()> {
    let out = substitute_template(
        DEFAULT_CONFIG_TEMPLATE,
        "open-ai",
        Some("https://example.com/v1"),
        Some("sk-abc"),
    );
    let config: TcodeConfig = toml::from_str(&out)?;
    assert_eq!(config.provider.as_deref(), Some("open-ai"));
    assert_eq!(config.base_url.as_deref(), Some("https://example.com/v1"));
    assert_eq!(config.api_key.as_deref(), Some("sk-abc"));

    // The original commented `# provider = "claude"` line must be gone,
    // replaced by an uncommented `provider = "open-ai"` assignment.
    assert!(
        !out.contains("# provider = \"claude\""),
        "expected commented provider line to be rewritten; got:\n{out}"
    );
    Ok(())
}

#[test]
fn test_substitute_only_provider() -> anyhow::Result<()> {
    let out = substitute_template(DEFAULT_CONFIG_TEMPLATE, "claude", None, None);
    let config: TcodeConfig = toml::from_str(&out)?;
    assert_eq!(config.provider.as_deref(), Some("claude"));
    assert!(config.api_key.is_none());
    assert!(config.base_url.is_none());
    Ok(())
}

#[test]
fn test_substitute_preserves_shortcuts() -> anyhow::Result<()> {
    let out = substitute_template(
        DEFAULT_CONFIG_TEMPLATE,
        "claude",
        Some("https://example.com"),
        Some("key"),
    );
    let config: TcodeConfig = toml::from_str(&out)?;
    assert!(
        !config.shortcuts.is_empty(),
        "expected [shortcuts] section to still parse with entries"
    );
    Ok(())
}

#[test]
fn test_substitute_provider_claude_produces_uncommented_claude() -> anyhow::Result<()> {
    let out = substitute_template(DEFAULT_CONFIG_TEMPLATE, "claude", None, None);
    let has_uncommented = out
        .lines()
        .any(|l| l.trim_start().starts_with("provider = \"claude\""));
    assert!(
        has_uncommented,
        "expected an uncommented `provider = \"claude\"` line; got:\n{out}"
    );
    Ok(())
}

#[test]
fn test_substitute_toml_escaping() -> anyhow::Result<()> {
    let api_key = r#"sk-ant-"weird\value"#;
    let base_url = r#"https://ex.com\path"with"quote"#;
    let out = substitute_template(
        DEFAULT_CONFIG_TEMPLATE,
        "claude",
        Some(base_url),
        Some(api_key),
    );
    let config: TcodeConfig = toml::from_str(&out)?;
    assert_eq!(config.api_key.as_deref(), Some(api_key));
    assert_eq!(config.base_url.as_deref(), Some(base_url));
    Ok(())
}

#[test]
fn test_substitute_roundtrip_through_layout_validation() -> anyhow::Result<()> {
    let out = substitute_template(
        DEFAULT_CONFIG_TEMPLATE,
        "open-router",
        Some("https://openrouter.ai/api/v1/custom"),
        Some("sk-or-xyz"),
    );
    // Exercise `deny_unknown_fields` — this catches regressions in the
    // template update if a new/renamed key sneaks in.
    let config: TcodeConfig = toml::from_str(&out)?;
    // The template ships with `[layout]` fully commented out.
    assert!(config.layout.is_none());
    if let Some(layout) = &config.layout {
        layout.validate()?;
    }
    Ok(())
}

#[test]
fn test_substitute_rewrites_all_three_keys_in_default_template() -> anyhow::Result<()> {
    let rendered = substitute_template(
        DEFAULT_CONFIG_TEMPLATE,
        "open-ai",
        Some("https://example.test/v1"),
        Some("sk-test"),
    );

    // Assert each of the three keys produces an uncommented line that
    // starts the line (not preceded by `#`).
    let has_line = |needle: &str| -> bool { rendered.lines().any(|l| l.starts_with(needle)) };

    assert!(
        has_line("provider = "),
        "provider was not rewritten to an uncommented line. Template:\n{rendered}"
    );
    assert!(
        has_line("base_url = "),
        "base_url was not rewritten to an uncommented line. Template:\n{rendered}"
    );
    assert!(
        has_line("api_key = "),
        "api_key was not rewritten to an uncommented line. Template:\n{rendered}"
    );

    Ok(())
}

/// Empty `api_key` input from the wizard writes an uncommented
/// `api_key = ""` line to the config file and parses back as
/// `Some("")`, not `None`. This is the key behavioral change for empty
/// API key support.
#[test]
fn test_substitute_empty_api_key_round_trips_as_some_empty() -> anyhow::Result<()> {
    let out = substitute_template(DEFAULT_CONFIG_TEMPLATE, "claude", None, Some(""));
    let config: TcodeConfig = toml::from_str(&out)?;
    assert_eq!(config.provider.as_deref(), Some("claude"));
    assert_eq!(
        config.api_key.as_deref(),
        Some(""),
        "expected empty api_key to round-trip as Some(\"\"); got {:?}",
        config.api_key
    );

    // The rendered output must contain an uncommented `api_key = ""` line
    // (i.e. `try_rewrite` fired and replaced the commented example).
    // `try_rewrite` always emits `key = value` with spaces around `=`.
    let has_uncommented = out.lines().any(|l| l.starts_with("api_key = \"\""));
    assert!(
        has_uncommented,
        "expected an uncommented `api_key = \"\"` line; got:\n{out}"
    );

    // And the original commented example line is gone.
    assert!(
        !out.contains("# api_key = \"\""),
        "expected commented api_key example to be rewritten; got:\n{out}"
    );
    Ok(())
}

/// The `claude-oauth` wizard choice writes `provider = "claude-oauth"`
/// and skips both the base URL and API key prompts — the rendered config
/// contains neither an uncommented `base_url` nor `api_key` line.
#[test]
fn test_substitute_claude_oauth_provider() -> anyhow::Result<()> {
    let out = substitute_template(DEFAULT_CONFIG_TEMPLATE, "claude-oauth", None, None);
    let config: TcodeConfig = toml::from_str(&out)?;
    assert_eq!(config.provider.as_deref(), Some("claude-oauth"));
    assert!(
        config.api_key.is_none(),
        "expected api_key to be None for claude-oauth; got {:?}",
        config.api_key
    );
    assert!(
        config.base_url.is_none(),
        "expected base_url to be None for claude-oauth; got {:?}",
        config.base_url
    );
    Ok(())
}
