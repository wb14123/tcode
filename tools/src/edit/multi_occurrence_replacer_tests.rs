use crate::edit::multi_occurrence_replacer::MultiOccurrenceReplacer;

#[test]
fn multi_replaces_all_occurrences() -> anyhow::Result<()> {
    let content = "foo bar foo baz foo\n";
    let result = MultiOccurrenceReplacer::replace(content, "foo", "qux", true)?;
    assert_eq!(result, "qux bar qux baz qux\n");
    Ok(())
}

#[test]
fn multi_single_occurrence_replace_all() -> anyhow::Result<()> {
    let content = "hello world\n";
    let result = MultiOccurrenceReplacer::replace(content, "hello", "hi", true)?;
    assert_eq!(result, "hi world\n");
    Ok(())
}

#[test]
fn multi_rejects_when_replace_all_is_false() {
    let content = "hello world\n";
    let result = MultiOccurrenceReplacer::replace(content, "hello", "hi", false);
    assert!(result.is_err());
}

#[test]
fn multi_not_found_error() {
    let content = "hello world\n";
    let result = MultiOccurrenceReplacer::replace(content, "goodbye", "hi", true);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn multi_multiline_pattern() -> anyhow::Result<()> {
    let content = "aaa\nbbb\naaa\nbbb\n";
    let result = MultiOccurrenceReplacer::replace(content, "aaa\nbbb", "ccc\nddd", true)?;
    assert_eq!(result, "ccc\nddd\nccc\nddd\n");
    Ok(())
}

#[test]
fn multi_overlapping_patterns() -> anyhow::Result<()> {
    // "aaa" appears 3 times in "aaaa" but replace replaces non-overlapping
    let content = "aaaa\n";
    let result = MultiOccurrenceReplacer::replace(content, "aa", "b", true)?;
    assert_eq!(result, "bb\n");
    Ok(())
}

#[test]
fn multi_empty_replacement() -> anyhow::Result<()> {
    let content = "hello world hello\n";
    let result = MultiOccurrenceReplacer::replace(content, "hello", "", true)?;
    assert_eq!(result, " world \n");
    Ok(())
}

#[test]
fn multi_preserves_non_matching_content() -> anyhow::Result<()> {
    let content = "line1\nfoo\nline3\nfoo\nline5\n";
    let result = MultiOccurrenceReplacer::replace(content, "foo", "bar", true)?;
    assert_eq!(result, "line1\nbar\nline3\nbar\nline5\n");
    Ok(())
}

#[test]
fn multi_special_characters() -> anyhow::Result<()> {
    let content = "a.b.c.d\n";
    let result = MultiOccurrenceReplacer::replace(content, ".", "-", true)?;
    assert_eq!(result, "a-b-c-d\n");
    Ok(())
}

#[test]
fn multi_replace_with_longer_string() -> anyhow::Result<()> {
    let content = "ab\n";
    let result = MultiOccurrenceReplacer::replace(content, "a", "xyz", true)?;
    assert_eq!(result, "xyzb\n");
    Ok(())
}
