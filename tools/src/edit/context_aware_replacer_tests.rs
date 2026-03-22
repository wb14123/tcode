use crate::edit::context_aware_replacer::ContextAwareReplacer;

#[test]
fn context_basic_similar_block() -> anyhow::Result<()> {
    let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n    let z = 3;\n}\n";
    // old_string with minor differences in some lines
    let old = "fn foo() {\n    let x = 1;\n    let yy = 22;\n    let z = 3;\n}";
    let new = "fn bar() {\n    let a = 10;\n    let b = 20;\n    let c = 30;\n}";
    // 4 out of 5 lines match (80%), which is >= 50%
    let result = ContextAwareReplacer::replace(content, old, new, false)?;
    assert_eq!(
        result,
        "fn bar() {\n    let a = 10;\n    let b = 20;\n    let c = 30;\n}\n"
    );
    Ok(())
}

#[test]
fn context_not_enough_similarity() {
    let content = "fn foo() {\n    let x = 1;\n    let y = 2;\n    let z = 3;\n}\n";
    // Completely unrelated lines — no line should reach 80% similarity
    let old = "import numpy as np\nclass DataProcessor:\n    def __init__(self):\n        self.data = []\n        pass";
    let result = ContextAwareReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
}

#[test]
fn context_empty_old_error() {
    let content = "some content\n";
    let result = ContextAwareReplacer::replace(content, "", "replacement", false);
    assert!(result.is_err());
}

#[test]
fn context_single_line_match() -> anyhow::Result<()> {
    // Single line: needs 1 line to match (ceil(1 * 0.5) = 1)
    let content = "let x = 1;\n";
    let old = "let x = 1;";
    let new = "let x = 2;";
    let result = ContextAwareReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "let x = 2;\n");
    Ok(())
}

#[test]
fn context_multiple_matches_error() {
    let content = "let x = 1;\nother\nlet x = 1;\n";
    let old = "let x = 1;";
    let result = ContextAwareReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("2 matching blocks")
    );
}

#[test]
fn context_replace_all() -> anyhow::Result<()> {
    let content = "let x = 1;\nother\nlet x = 1;\n";
    let old = "let x = 1;";
    let new = "let x = 99;";
    let result = ContextAwareReplacer::replace(content, old, new, true)?;
    assert_eq!(result, "let x = 99;\nother\nlet x = 99;\n");
    Ok(())
}

#[test]
fn context_fifty_percent_threshold() -> anyhow::Result<()> {
    // 4 lines, need at least ceil(4 * 0.5) = 2 matching
    let content = "line_a\nline_b\nline_c\nline_d\n";
    // 2 out of 4 lines match exactly: line_a and line_d
    let old = "line_a\ntotally_different_1\ntotally_different_2\nline_d";
    let new = "new_a\nnew_b\nnew_c\nnew_d";
    let result = ContextAwareReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "new_a\nnew_b\nnew_c\nnew_d\n");
    Ok(())
}

#[test]
fn context_below_threshold_fails() {
    // 4 lines, need at least 2 matching
    let content = "line_a\nline_b\nline_c\nline_d\n";
    // Only 1 out of 4 lines matches: line_a
    let old = "line_a\nxxx\nyyy\nzzz";
    let result = ContextAwareReplacer::replace(content, old, "replacement", false);
    assert!(result.is_err());
}

#[test]
fn context_preserves_trailing_newline() -> anyhow::Result<()> {
    let content = "start\nhello world\nend\n";
    let old = "hello world";
    let new = "goodbye world";
    let result = ContextAwareReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "start\ngoodbye world\nend\n");
    Ok(())
}

#[test]
fn context_no_trailing_newline() -> anyhow::Result<()> {
    let content = "start\nhello world\nend";
    let old = "hello world";
    let new = "goodbye world";
    let result = ContextAwareReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "start\ngoodbye world\nend");
    Ok(())
}

#[test]
fn context_with_indentation_differences() -> anyhow::Result<()> {
    // Lines match after trimming (similarity should be high)
    let content = "    fn foo() {\n        let x = 1;\n    }\n";
    let old = "fn foo() {\n    let x = 1;\n}";
    let new = "    fn bar() {\n        let x = 2;\n    }";
    let result = ContextAwareReplacer::replace(content, old, new, false)?;
    assert_eq!(result, "    fn bar() {\n        let x = 2;\n    }\n");
    Ok(())
}
