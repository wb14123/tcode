use crate::edit::indentation_flexible_replacer::IndentationFlexibleReplacer;

#[test]
fn indent_basic_offset() -> anyhow::Result<()> {
    // Content has 4-space indent, old_string has no indent
    let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
    let old = "let x = 1;\nlet y = 2;";
    let new = "    let x = 10;\n    let y = 20;";
    let result = IndentationFlexibleReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "fn foo() {\n    let x = 10;\n    let y = 20;\n}\n");
    Ok(())
}

#[test]
fn indent_different_indent_levels() -> anyhow::Result<()> {
    // Content has 8-space indent, old_string has 2-space indent
    let content = "fn foo() {\n        let x = 1;\n        let y = 2;\n}\n";
    let old = "  let x = 1;\n  let y = 2;";
    let new = "        let x = 10;\n        let y = 20;";
    let result = IndentationFlexibleReplacer::replace(content, old, new, false)?;
    assert_eq!(
        result,
        "fn foo() {\n        let x = 10;\n        let y = 20;\n}\n"
    );
    Ok(())
}

#[test]
fn indent_tabs_vs_spaces() -> anyhow::Result<()> {
    let content = "\t\tlet x = 1;\n\t\tlet y = 2;\n";
    // old_string uses 2 spaces instead of tabs (both strip to same relative indent = 0)
    let old = "  let x = 1;\n  let y = 2;";
    let new = "\t\tlet x = 10;\n\t\tlet y = 20;";
    let result = IndentationFlexibleReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "\t\tlet x = 10;\n\t\tlet y = 20;\n");
    Ok(())
}

#[test]
fn indent_not_found() {
    let content = "let x = 1;\nlet y = 2;\n";
    let old = "let a = 3;\nlet b = 4;";
    let result = IndentationFlexibleReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no match found"));
}

#[test]
fn indent_multiple_matches_error() {
    let content = "    let x = 1;\n    let x = 1;\n";
    let old = "let x = 1;";
    let result = IndentationFlexibleReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("2 matches"));
}

#[test]
fn indent_replace_all() -> anyhow::Result<()> {
    let content = "    let x = 1;\nother\n    let x = 1;\n";
    let old = "let x = 1;";
    let new = "let x = 99;";
    let result = IndentationFlexibleReplacer::replace(content, old, new, true)?;
    assert_eq!(result, "let x = 99;\nother\nlet x = 99;\n");
    Ok(())
}

#[test]
fn indent_empty_old_error() {
    let content = "some content\n";
    let result = IndentationFlexibleReplacer::replace(content, "", "replacement", false);
    assert!(result.is_err());
}

#[test]
fn indent_preserves_trailing_newline() -> anyhow::Result<()> {
    let content = "    hello\n";
    let old = "hello";
    let new = "    world";
    let result = IndentationFlexibleReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "    world\n");
    Ok(())
}

#[test]
fn indent_mixed_empty_lines() -> anyhow::Result<()> {
    // Empty lines should be handled gracefully (not affect min_indent calculation)
    let content = "    fn foo() {\n\n        let x = 1;\n    }\n";
    let old = "fn foo() {\n\n    let x = 1;\n}";
    let new = "    fn bar() {\n\n        let x = 2;\n    }";
    let result = IndentationFlexibleReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "    fn bar() {\n\n        let x = 2;\n    }\n");
    Ok(())
}

#[test]
fn indent_same_indent_still_matches() -> anyhow::Result<()> {
    // When old_string already has the same indent as content, it should still match
    let content = "    let x = 1;\n    let y = 2;\n";
    let old = "    let x = 1;\n    let y = 2;";
    let new = "    let x = 10;\n    let y = 20;";
    let result = IndentationFlexibleReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "    let x = 10;\n    let y = 20;\n");
    Ok(())
}
