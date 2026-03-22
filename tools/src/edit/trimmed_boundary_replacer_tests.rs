use crate::edit::trimmed_boundary_replacer::TrimmedBoundaryReplacer;

#[test]
fn boundary_leading_whitespace() -> anyhow::Result<()> {
    let content = "hello world\n";
    let old = "  hello world  "; // extra spaces around
    let new = "hi world";
    let result = TrimmedBoundaryReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "hi world\n");
    Ok(())
}

#[test]
fn boundary_leading_newlines() -> anyhow::Result<()> {
    let content = "hello world\n";
    let old = "\n\nhello world\n\n"; // extra newlines around
    let new = "hi world";
    let result = TrimmedBoundaryReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "hi world\n");
    Ok(())
}

#[test]
fn boundary_no_trimming_needed_error() {
    let content = "hello world\n";
    let old = "hello world"; // already trimmed
    let result = TrimmedBoundaryReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("no leading/trailing whitespace")
    );
}

#[test]
fn boundary_not_found() {
    let content = "hello world\n";
    let old = "  goodbye world  ";
    let result = TrimmedBoundaryReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no match found"));
}

#[test]
fn boundary_multiple_matches_error() {
    let content = "aaa bbb aaa\n";
    let old = " aaa ";
    let result = TrimmedBoundaryReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("2 matches"));
}

#[test]
fn boundary_replace_all() -> anyhow::Result<()> {
    let content = "aaa bbb aaa\n";
    let old = " aaa ";
    let new = "ccc";
    let result = TrimmedBoundaryReplacer::replace(content, old, new, true)?;
    assert_eq!(result, "ccc bbb ccc\n");
    Ok(())
}

#[test]
fn boundary_empty_after_trim_error() {
    let content = "hello\n";
    let old = "   \n  \t  ";
    let result = TrimmedBoundaryReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("empty after trimming")
    );
}

#[test]
fn boundary_tabs_and_spaces() -> anyhow::Result<()> {
    let content = "let x = 1;\n";
    let old = "\t let x = 1; \t";
    let new = "let x = 2;";
    let result = TrimmedBoundaryReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "let x = 2;\n");
    Ok(())
}

#[test]
fn boundary_multiline_content() -> anyhow::Result<()> {
    let content = "fn foo() {\n    let x = 1;\n}\n";
    let old = "\nfn foo() {\n    let x = 1;\n}\n";
    let new = "fn bar() {\n    let x = 2;\n}";
    let result = TrimmedBoundaryReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "fn bar() {\n    let x = 2;\n}\n");
    Ok(())
}
