use crate::edit::replacer::Replacer;

const NOT_FOUND: &str = "old_string was not found in the file. Make sure it matches \
                          exactly, including whitespace and indentation.";

#[test]
fn simple_exact_match() -> anyhow::Result<()> {
    let content = "fn main() {\n    println!(\"hello\");\n}\n";
    let result =
        Replacer::replace_exact(content, "println!(\"hello\")", "println!(\"world\")", false)?;
    assert_eq!(result, "fn main() {\n    println!(\"world\");\n}\n");
    Ok(())
}

#[test]
fn simple_not_found() {
    let content = "fn main() {}\n";
    let result = Replacer::replace_exact(content, "nonexistent", "replacement", false);
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
    let result = Replacer::replace_exact(content, "aaa", "ccc", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("2 times"));
}

#[test]
fn simple_replace_all() -> anyhow::Result<()> {
    let content = "aaa\nbbb\naaa\n";
    let result = Replacer::replace_exact(content, "aaa", "ccc", true)?;
    assert_eq!(result, "ccc\nbbb\nccc\n");
    Ok(())
}

#[test]
fn simple_identical_strings_still_works() -> anyhow::Result<()> {
    // The edit tool itself checks old==new, but the replacer shouldn't care
    let content = "hello world\n";
    let result = Replacer::replace_exact(content, "hello", "hello", false)?;
    assert_eq!(result, "hello world\n");
    Ok(())
}

#[test]
fn crlf_file_lf_search() -> anyhow::Result<()> {
    let content = "line one\r\nline two\r\nline three\r\n";
    let (result, count) = Replacer::replace(
        content,
        "line two\nline three",
        "line two\nLINE THREE",
        false,
    )?;
    assert_eq!(result, "line one\r\nline two\r\nLINE THREE\r\n");
    assert_eq!(count, 1);
    assert!(!result.contains("\n\n"));
    assert_eq!(result.matches("\r\n").count(), 3);
    assert_eq!(result.matches('\n').count(), result.matches("\r\n").count());
    Ok(())
}

#[test]
fn crlf_replace_adds_crlf() -> anyhow::Result<()> {
    let content = "a\r\nb\r\n";
    let (result, count) = Replacer::replace(content, "a\nb", "x\ny", false)?;
    assert_eq!(result, "x\r\ny\r\n");
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn count_reflects_successful_attempt() -> anyhow::Result<()> {
    let content = "foo\r\nbar\r\nfoo\r\nbar\r\nfoo\r\nbar\r\n";
    let (result, count) = Replacer::replace(content, "foo\nbar", "baz\nqux", true)?;
    assert_eq!(result, "baz\r\nqux\r\nbaz\r\nqux\r\nbaz\r\nqux\r\n");
    assert_eq!(count, 3);
    Ok(())
}

#[test]
fn exact_attempt_wins() -> anyhow::Result<()> {
    // The search text matches once with Unix endings and twice with Windows
    // endings, so the exact attempt must win rather than be reported as ambiguous.
    let content = "key\nvalue\r\nkey\r\nvalue\r\nkey\r\nvalue\r\n";
    let (result, count) = Replacer::replace(content, "key\nvalue", "K\nV", false)?;
    assert_eq!(result, "K\nV\r\nkey\r\nvalue\r\nkey\r\nvalue\r\n");
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn ambiguous_exact_attempt_does_not_fall_through() {
    // The search text matches twice as given and only once with Windows endings, so
    // a retry on ambiguity would silently edit the wrong occurrence.
    let content = "x\ny\r\nx\ny\r\nx\r\ny\r\n";
    let error = Replacer::replace(content, "x\ny", "Q", false)
        .expect_err("an ambiguous exact search must not be retried");
    assert_eq!(
        error.to_string(),
        "old_string appears 2 times in the file. Provide more surrounding context to make it \
         unique, or set replace_all to true."
    );
}

#[test]
fn not_found_after_both_attempts() {
    let content = "hello world\n";
    let error = Replacer::replace(content, "goodbye\nmoon", "greetings\nmoon", false)
        .expect_err("absent text must not be found");
    assert_eq!(error.to_string(), NOT_FOUND);
}

#[test]
fn ambiguous_after_conversion() {
    let content = "a\r\nb\r\na\r\nb\r\n";
    let error = Replacer::replace(content, "a\nb", "c\nd", false)
        .expect_err("a search matching twice must be ambiguous");
    assert_eq!(
        error.to_string(),
        "old_string appears 2 times in the file. Provide more surrounding context to make it \
         unique, or set replace_all to true."
    );
}

#[test]
fn mixed_region_not_found() {
    let content = "alpha\r\nbeta\ngamma\r\n";
    let error = Replacer::replace(content, "alpha\nbeta\ngamma", "A\nB\nC", false)
        .expect_err("a region with mixed endings must not be found");
    assert_eq!(error.to_string(), NOT_FOUND);
}

#[test]
fn crlf_search_text_on_lf_file_not_found() {
    let content = "alpha\nbeta\n";
    let error = Replacer::replace(content, "alpha\r\nbeta", "A\r\nB", false)
        .expect_err("a search text with Windows endings must not be found in a Unix file");
    assert_eq!(error.to_string(), NOT_FOUND);
}

#[test]
fn single_line_search_keeps_replacement_as_supplied() -> anyhow::Result<()> {
    let content = "one\r\ntwo\r\nthree\r\n";
    let (result, count) = Replacer::replace(content, "two", "two\nand a half", false)?;
    assert_eq!(result, "one\r\ntwo\nand a half\r\nthree\r\n");
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn conversion_is_idempotent() {
    // A search text that already contains `\r\n` must not be converted into
    // `\r\r\n`: the correctly converted search text does not match this content,
    // while a doubly converted one would.
    let content = "a\r\r\nb\r\n";
    let error = Replacer::replace(content, "a\r\nb", "x", false)
        .expect_err("a doubly converted search text would have matched");
    assert_eq!(error.to_string(), NOT_FOUND);
}

#[test]
fn line_ending_only_edit_leaves_content_unchanged() -> anyhow::Result<()> {
    // Search and replacement differ only in line endings, so the conversion makes
    // them identical and the returned content is exactly the input. Callers treat
    // that as a no-op rather than as an edit.
    let content = "a\r\nb\r\n";
    let (result, count) = Replacer::replace(content, "a\nb", "a\r\nb", false)?;
    assert_eq!(result, content);
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn lone_cr_is_left_alone() -> anyhow::Result<()> {
    // A lone `\r` in the content is not a line ending and stays as it is.
    let content = "one\rtwo\r\nthree\n";
    let (result, count) = Replacer::replace(content, "two\nthree", "TWO\nTHREE", false)?;
    assert_eq!(result, "one\rTWO\r\nTHREE\n");
    assert_eq!(count, 1);

    // A lone `\r` in the search text is not turned into a line ending either.
    let content = "a\rb\r\nc\n";
    let (result, count) = Replacer::replace(content, "a\rb\nc", "X\rY\nZ", false)?;
    assert_eq!(result, "X\rY\r\nZ\n");
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn untouched_regions_are_byte_identical() -> anyhow::Result<()> {
    let content = "HEAD\r\n\r\nkeep\r\nfoo\r\nbar\r\nTAIL";
    let (result, count) = Replacer::replace(content, "foo\nbar", "FOO\nBAR", false)?;
    assert_eq!(result, "HEAD\r\n\r\nkeep\r\nFOO\r\nBAR\r\nTAIL");
    assert_eq!(count, 1);
    assert_eq!(&result.as_bytes()[..14], &content.as_bytes()[..14]);
    assert_eq!(
        &result.as_bytes()[result.len() - 4..],
        &content.as_bytes()[content.len() - 4..]
    );
    assert!(!result.ends_with('\n'));
    Ok(())
}

#[test]
fn windows_edit_never_produces_double_cr() -> anyhow::Result<()> {
    let content = "fn main() {\r\n    let x = 1;\r\n}\r\n";
    let (result, count) = Replacer::replace(
        content,
        "    let x = 1;\n}",
        "    let x = 1;\n    let y = 2;\n}",
        false,
    )?;
    assert_eq!(count, 1);
    assert_eq!(
        result.as_bytes(),
        b"fn main() {\r\n    let x = 1;\r\n    let y = 2;\r\n}\r\n"
    );
    // Byte comparison: the inserted lines carry the file's endings and no
    // `\r\r\n` sequence appears anywhere in the result.
    assert!(
        !result
            .as_bytes()
            .windows(3)
            .any(|w| w == b"\r\r\n".as_slice())
    );
    Ok(())
}
