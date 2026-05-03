use super::openrouter::{Usage, extract_usage};

#[test]
fn extract_usage_splits_openrouter_prompt_cache_tokens() -> anyhow::Result<()> {
    let usage: Usage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 10339,
        "completion_tokens": 60,
        "prompt_tokens_details": {
            "cached_tokens": 10318,
            "cache_write_tokens": 20
        },
        "output_tokens_details": {
            "reasoning_tokens": 7
        }
    }))?;

    let (input_tokens, output_tokens, reasoning_tokens, cache_creation_tokens, cache_read_tokens) =
        extract_usage(&usage);

    assert_eq!(input_tokens, 1);
    assert_eq!(output_tokens, 60);
    assert_eq!(reasoning_tokens, 7);
    assert_eq!(cache_creation_tokens, 20);
    assert_eq!(cache_read_tokens, 10318);

    Ok(())
}

#[test]
fn extract_usage_defaults_cache_tokens_when_details_missing() -> anyhow::Result<()> {
    let usage: Usage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 123,
        "completion_tokens": 45
    }))?;

    let (input_tokens, output_tokens, reasoning_tokens, cache_creation_tokens, cache_read_tokens) =
        extract_usage(&usage);

    assert_eq!(input_tokens, 123);
    assert_eq!(output_tokens, 45);
    assert_eq!(reasoning_tokens, 0);
    assert_eq!(cache_creation_tokens, 0);
    assert_eq!(cache_read_tokens, 0);

    Ok(())
}

#[test]
fn extract_usage_clamps_input_tokens_if_cache_details_exceed_prompt_tokens() -> anyhow::Result<()> {
    let usage: Usage = serde_json::from_value(serde_json::json!({
        "prompt_tokens": 10,
        "completion_tokens": 2,
        "prompt_tokens_details": {
            "cached_tokens": 9,
            "cache_write_tokens": 9
        }
    }))?;

    let (input_tokens, output_tokens, reasoning_tokens, cache_creation_tokens, cache_read_tokens) =
        extract_usage(&usage);

    assert_eq!(input_tokens, 0);
    assert_eq!(output_tokens, 2);
    assert_eq!(reasoning_tokens, 0);
    assert_eq!(cache_creation_tokens, 9);
    assert_eq!(cache_read_tokens, 9);

    Ok(())
}
