use std::sync::{Arc, LazyLock, Mutex};

use headless_chrome::{Browser, LaunchOptions};

/// The shared cleaning JS from clean-html.js. Defines `cleanHtml(html)` in the page scope.
const CLEAN_HTML_JS: &str = include_str!("clean-html.js");

/// Shared browser launch result. `Err` stores the message when Chrome is not found.
static BROWSER_RESULT: LazyLock<Result<Mutex<Browser>, String>> = LazyLock::new(|| {
    match Browser::new(LaunchOptions {
        headless: true,
        ..LaunchOptions::default()
    }) {
        Ok(browser) => Ok(Mutex::new(browser)),
        Err(e) => Err(format!("Chrome not available — install Chrome/Chromium to run these tests: {e}")),
    }
});

fn new_tab() -> Arc<headless_chrome::Tab> {
    let mutex = match BROWSER_RESULT.as_ref() {
        Ok(m) => m,
        Err(msg) => panic!("{msg}"),
    };
    let browser = mutex.lock().expect("browser mutex poisoned");
    let tab = browser.new_tab().expect("failed to open tab");
    tab.navigate_to("about:blank").unwrap();
    tab.wait_until_navigated().unwrap();
    // Define cleanHtml in the page scope so clean() can call it
    tab.evaluate(CLEAN_HTML_JS, false)
        .expect("failed to load clean-html.js");
    tab
}

/// Call the real cleanHtml function from clean-html.js.
fn clean(tab: &headless_chrome::Tab, html: &str) -> String {
    let escaped = serde_json::to_string(html).unwrap();
    let js = format!("cleanHtml({escaped})");
    let result = tab.evaluate(&js, false).expect("JS evaluation failed");
    match &result.value {
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => panic!("expected string from cleanHtml, got: {result:?}"),
    }
}

#[test]
fn bare_span_unwrapped() {
    let tab = new_tab();
    assert_eq!(clean(&tab, "<span>hello</span>"), "hello");
}

#[test]
fn span_with_class_stripped_and_unwrapped() {
    let tab = new_tab();
    let input = r#"<span class="highlight">hello</span>"#;
    // class is stripped, then bare span is unwrapped
    assert_eq!(clean(&tab, input), "hello");
}

#[test]
fn nested_spans_unwrapped() {
    let tab = new_tab();
    assert_eq!(clean(&tab, "<span><span>deep</span></span>"), "deep");
}

#[test]
fn div_wrapping_single_div_flattened() {
    let tab = new_tab();
    let input = "<div><div><p>text</p></div></div>";
    assert_eq!(clean(&tab, input), "<div><p>text</p></div>");
}

#[test]
fn deeply_nested_divs_flattened() {
    let tab = new_tab();
    let input = "<div><div><div><p>text</p></div></div></div>";
    assert_eq!(clean(&tab, input), "<div><p>text</p></div>");
}

#[test]
fn div_with_inline_content_unwrapped() {
    let tab = new_tab();
    assert_eq!(clean(&tab, "<div>just text</div>"), "just text");
}

#[test]
fn div_with_inline_elements_unwrapped() {
    let tab = new_tab();
    assert_eq!(
        clean(&tab, "<div><strong>bold</strong> and <em>italic</em></div>"),
        "<strong>bold</strong> and <em>italic</em>"
    );
}

#[test]
fn div_with_block_children_kept() {
    let tab = new_tab();
    let input = "<div><p>one</p><p>two</p></div>";
    assert_eq!(clean(&tab, input), input);
}

#[test]
fn div_with_class_stripped() {
    let tab = new_tab();
    let input = r#"<div class="container"><p>text</p></div>"#;
    // class is stripped; div with single block child stays (not single-div or all-inline)
    assert_eq!(clean(&tab, input), "<div><p>text</p></div>");
}

#[test]
fn semantic_elements_preserved() {
    let tab = new_tab();
    let input = "<h1>Title</h1><ul><li>item</li></ul><pre><code>x</code></pre>";
    assert_eq!(clean(&tab, input), input);
}

#[test]
fn links_preserved() {
    let tab = new_tab();
    let input = r#"<a href="https://example.com">link</a>"#;
    assert_eq!(clean(&tab, input), input);
}

#[test]
fn mixed_real_world_cleanup() {
    let tab = new_tab();
    let input = "<div><div><div><span>Hello</span> <span>world</span></div></div><p>Paragraph</p></div>";
    let result = clean(&tab, input);
    assert!(!result.contains("<span>"), "bare spans should be removed");
    assert!(result.contains("<p>Paragraph</p>"), "p should be preserved");
    assert!(result.contains("Hello"), "text content preserved");
}

#[test]
fn data_uri_img_removed() {
    let tab = new_tab();
    let input = r#"<p>before<img src="data:image/png;base64,AAAA">after</p>"#;
    assert_eq!(clean(&tab, input), "<p>beforeafter</p>");
}

#[test]
fn svg_removed() {
    let tab = new_tab();
    let input = r#"<p>text<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M0 0h24v24H0z"></path></svg></p>"#;
    assert_eq!(clean(&tab, input), "<p>text</p>");
}

#[test]
fn style_and_class_stripped() {
    let tab = new_tab();
    let input = r#"<p class="intro" style="color:red" id="p1">text</p>"#;
    assert_eq!(clean(&tab, input), "<p>text</p>");
}

#[test]
fn data_attributes_stripped() {
    let tab = new_tab();
    let input = r#"<p data-testid="foo" data-value="bar">text</p>"#;
    assert_eq!(clean(&tab, input), "<p>text</p>");
}

#[test]
fn srcset_stripped() {
    let tab = new_tab();
    let input = r#"<img src="photo.jpg" srcset="photo-2x.jpg 2x">"#;
    assert_eq!(clean(&tab, input), r#"<img src="photo.jpg">"#);
}

#[test]
fn link_keeps_href_strips_rel() {
    let tab = new_tab();
    let input = r#"<a href="https://example.com" rel="noopener" target="_blank">link</a>"#;
    assert_eq!(clean(&tab, input), r#"<a href="https://example.com">link</a>"#);
}

#[test]
fn noscript_removed() {
    let tab = new_tab();
    let input = "<p>text</p><noscript><p>fallback</p></noscript>";
    assert_eq!(clean(&tab, input), "<p>text</p>");
}

#[test]
fn picture_keeps_img_fallback() {
    let tab = new_tab();
    let input = r#"<picture><source srcset="photo.webp" type="image/webp"><img src="photo.jpg"></picture>"#;
    assert_eq!(clean(&tab, input), r#"<img src="photo.jpg">"#);
}

#[test]
fn aria_and_role_stripped() {
    let tab = new_tab();
    let input = r#"<p aria-label="description" role="button">text</p>"#;
    assert_eq!(clean(&tab, input), "<p>text</p>");
}

#[test]
fn html_comments_removed() {
    let tab = new_tab();
    let input = "<p>text</p><!-- comment --><p>more</p>";
    assert_eq!(clean(&tab, input), "<p>text</p><p>more</p>");
}

#[test]
fn img_loading_attrs_stripped() {
    let tab = new_tab();
    let input = r#"<img src="photo.jpg" loading="lazy" decoding="async" fetchpriority="low" width="100" height="200">"#;
    assert_eq!(clean(&tab, input), r#"<img src="photo.jpg">"#);
}

#[test]
fn empty_src_img_removed() {
    let tab = new_tab();
    let input = r#"<p>before<img src="">after</p>"#;
    assert_eq!(clean(&tab, input), "<p>beforeafter</p>");
}
