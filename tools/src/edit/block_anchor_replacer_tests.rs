use crate::edit::block_anchor_replacer::BlockAnchorReplacer;

#[test]
fn anchor_basic_match() -> anyhow::Result<()> {
    let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
    let old = "fn foo() {\n    let x = 1;\n    let y = 2;\n}";
    let new = "fn foo() {\n    let x = 10;\n    let y = 20;\n}";
    let result = BlockAnchorReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "fn foo() {\n    let x = 10;\n    let y = 20;\n}\n");
    Ok(())
}

#[test]
fn anchor_with_similar_middle_lines() -> anyhow::Result<()> {
    // Middle lines have minor differences (indentation) but same trimmed content
    let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
    let old = "fn foo() {\n  let x = 1;\n  let y = 2;\n}"; // different indentation
    let new = "fn bar() {\n    let x = 10;\n    let y = 20;\n}";
    let result = BlockAnchorReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "fn bar() {\n    let x = 10;\n    let y = 20;\n}\n");
    Ok(())
}

#[test]
fn anchor_middle_too_different() {
    let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n}\n";
    let old = "fn foo() {\n    completely different code;\n}";
    let result = BlockAnchorReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
}

#[test]
fn anchor_not_found() {
    let content = "fn foo() {\n    let x = 1;\n}\n";
    let old = "fn bar() {\n    let x = 1;\n}";
    let result = BlockAnchorReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
}

#[test]
fn anchor_two_line_block() -> anyhow::Result<()> {
    // Only first + last, no middle
    let content = "fn foo() {\n}\n";
    let old = "fn foo() {\n}";
    let new = "fn bar() {\n}";
    let result = BlockAnchorReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "fn bar() {\n}\n");
    Ok(())
}

#[test]
fn anchor_single_line_error() {
    let content = "fn foo() {}\n";
    let old = "fn foo() {}";
    let result = BlockAnchorReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("at least 2 lines"));
}

#[test]
fn anchor_multiple_matches_error() {
    let content = "fn foo() {\n    let x = 1;\n}\nfn foo() {\n    let x = 1;\n}\n";
    let old = "fn foo() {\n    let x = 1;\n}";
    let result = BlockAnchorReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("2 matching blocks")
    );
}

#[test]
fn anchor_replace_all() -> anyhow::Result<()> {
    let content = "fn foo() {\n    let x = 1;\n}\nfn foo() {\n    let x = 1;\n}\n";
    let old = "fn foo() {\n    let x = 1;\n}";
    let new = "fn bar() {\n    let x = 2;\n}";
    let result = BlockAnchorReplacer::replace(content, old, new, true)?;
    assert_eq!(
        result,
        "fn bar() {\n    let x = 2;\n}\nfn bar() {\n    let x = 2;\n}\n"
    );
    Ok(())
}

#[test]
fn anchor_empty_anchor_line_error() {
    let content = "fn foo() {\n    let x = 1;\n}\n";
    let old = "\n    let x = 1;\n}";
    let result = BlockAnchorReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
}

#[test]
fn anchor_preserves_trailing_newline() -> anyhow::Result<()> {
    let content = "start\nfn foo() {\n    body;\n}\nend\n";
    let old = "fn foo() {\n    body;\n}";
    let new = "fn bar() {\n    new_body;\n}";
    let result = BlockAnchorReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "start\nfn bar() {\n    new_body;\n}\nend\n");
    Ok(())
}
