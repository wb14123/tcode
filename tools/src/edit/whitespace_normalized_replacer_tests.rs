use crate::edit::whitespace_normalized_replacer::WhitespaceNormalizedReplacer;

#[test]
fn ws_basic_extra_spaces() -> anyhow::Result<()> {
    let content = "let  x  =  1;\n";
    let old = "let x = 1;";
    let new = "let x = 2;";
    let result = WhitespaceNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "let x = 2;\n");
    Ok(())
}

#[test]
fn ws_tabs_vs_spaces() -> anyhow::Result<()> {
    let content = "let\tx = 1;\n";
    let old = "let x = 1;";
    let new = "let x = 2;";
    let result = WhitespaceNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "let x = 2;\n");
    Ok(())
}

#[test]
fn ws_multiline_different_spacing() -> anyhow::Result<()> {
    let content = "fn foo() {\n    let  x = 1;\n    let  y = 2;\n}\n";
    let old = "fn foo() {\nlet x = 1;\nlet y = 2;\n}";
    let new = "fn bar() {\n    let x = 10;\n    let y = 20;\n}";
    let result = WhitespaceNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "fn bar() {\n    let x = 10;\n    let y = 20;\n}\n");
    Ok(())
}

#[test]
fn ws_not_found() {
    let content = "let x = 1;\n";
    let old = "let y = 2;";
    let result = WhitespaceNormalizedReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no match found"));
}

#[test]
fn ws_multiple_matches_error() {
    let content = "let  x = 1;\nother\nlet  x = 1;\n";
    let old = "let x = 1;";
    let result = WhitespaceNormalizedReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("2 matches"));
}

#[test]
fn ws_replace_all() -> anyhow::Result<()> {
    let content = "let  x = 1;\nother\nlet  x = 1;\n";
    let old = "let x = 1;";
    let new = "let x = 99;";
    let result = WhitespaceNormalizedReplacer::replace(content, old, new, true)?;
    assert_eq!(result, "let x = 99;\nother\nlet x = 99;\n");
    Ok(())
}

#[test]
fn ws_empty_old_error() {
    let content = "some content\n";
    let old = "   \n  \t  ";
    let result = WhitespaceNormalizedReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
}

#[test]
fn ws_preserves_trailing_newline() -> anyhow::Result<()> {
    let content = "hello  world\n";
    let old = "hello world";
    let new = "hi world";
    let result = WhitespaceNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "hi world\n");
    Ok(())
}

#[test]
fn ws_no_trailing_newline() -> anyhow::Result<()> {
    let content = "hello  world";
    let old = "hello world";
    let new = "hi world";
    let result = WhitespaceNormalizedReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "hi world");
    Ok(())
}
