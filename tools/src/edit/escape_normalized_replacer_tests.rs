use crate::edit::escape_normalized_replacer::EscapeNormalizedReplacer;

#[test]
fn escape_newline_sequence() -> anyhow::Result<()> {
    let content = "hello\nworld\n";
    let old = "hello\\nworld"; // literal \n in old_string
    let new = "hi\nworld";
    let result = EscapeNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "hi\nworld\n");
    Ok(())
}

#[test]
fn escape_tab_sequence() -> anyhow::Result<()> {
    let content = "hello\tworld\n";
    let old = "hello\\tworld"; // literal \t in old_string
    let new = "hello world";
    let result = EscapeNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "hello world\n");
    Ok(())
}

#[test]
fn escape_backslash_sequence() -> anyhow::Result<()> {
    let content = "path\\to\\file\n";
    let old = "path\\\\to\\\\file"; // escaped backslashes
    let new = "path/to/file";
    let result = EscapeNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "path/to/file\n");
    Ok(())
}

#[test]
fn escape_quote_sequence() -> anyhow::Result<()> {
    let content = "say \"hello\"\n";
    let old = "say \\\"hello\\\""; // escaped quotes
    let new = "say 'hello'";
    let result = EscapeNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "say 'hello'\n");
    Ok(())
}

#[test]
fn escape_no_sequences_error() {
    let content = "hello world\n";
    let old = "hello world"; // no escape sequences
    let result = EscapeNormalizedReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("no escape sequences")
    );
}

#[test]
fn escape_not_found_after_unescape() {
    let content = "hello world\n";
    let old = "goodbye\\nworld"; // unescapes to "goodbye\nworld" which isn't in content
    let result = EscapeNormalizedReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no match found"));
}

#[test]
fn escape_multiple_matches_error() {
    let content = "hello\nworld\nand\nhello\nworld\n";
    let old = "hello\\nworld"; // matches twice
    let result = EscapeNormalizedReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("2 matches"));
}

#[test]
fn escape_replace_all() -> anyhow::Result<()> {
    let content = "hello\nworld\nand\nhello\nworld\n";
    let old = "hello\\nworld";
    let new = "hi world";
    let result = EscapeNormalizedReplacer::replace(content, old, new, true)?;
    assert_eq!(result, "hi world\nand\nhi world\n");
    Ok(())
}

#[test]
fn escape_mixed_sequences() -> anyhow::Result<()> {
    let content = "line1\n\tindented\n";
    let old = "line1\\n\\tindented"; // \n and \t
    let new = "line1 indented";
    let result = EscapeNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "line1 indented\n");
    Ok(())
}

#[test]
fn escape_carriage_return() -> anyhow::Result<()> {
    let content = "hello\rworld\n";
    let old = "hello\\rworld";
    let new = "hello world";
    let result = EscapeNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "hello world\n");
    Ok(())
}

#[test]
fn escape_single_quote() -> anyhow::Result<()> {
    let content = "it's a test\n";
    let old = "it\\'s a test";
    let new = "its a test";
    let result = EscapeNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "its a test\n");
    Ok(())
}
