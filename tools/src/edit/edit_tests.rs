use crate::edit::exact_replacer::ExactReplacer;

#[test]
fn simple_exact_match() -> anyhow::Result<()> {
    let content = "fn main() {\n    println!(\"hello\");\n}\n";
    let result =
        ExactReplacer::replace(content, "println!(\"hello\")", "println!(\"world\")", false)?;
    assert_eq!(result, "fn main() {\n    println!(\"world\");\n}\n");
    Ok(())
}

#[test]
fn simple_not_found() {
    let content = "fn main() {}\n";
    let result = ExactReplacer::replace(content, "nonexistent", "replacement", false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not found in the file")
    );
}

#[test]
fn simple_multiple_matches_error() {
    let content = "aaa\nbbb\naaa\n";
    let result = ExactReplacer::replace(content, "aaa", "ccc", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("2 times"));
}

#[test]
fn simple_replace_all() -> anyhow::Result<()> {
    let content = "aaa\nbbb\naaa\n";
    let result = ExactReplacer::replace(content, "aaa", "ccc", true)?;
    assert_eq!(result, "ccc\nbbb\nccc\n");
    Ok(())
}

#[test]
fn simple_identical_strings_still_works() -> anyhow::Result<()> {
    // The edit tool itself checks old==new, but the replacer shouldn't care
    let content = "hello world\n";
    let result = ExactReplacer::replace(content, "hello", "hello", false)?;
    assert_eq!(result, "hello world\n");
    Ok(())
}
