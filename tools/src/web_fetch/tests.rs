use std::sync::{Arc, LazyLock, Mutex};

use headless_chrome::{Browser, LaunchOptions};

/// The cleaning logic from extract-content.js, as a standalone JS function.
/// Takes an HTML string argument and returns cleaned HTML.
const CLEAN_JS: &str = r#"
(function(html) {
    var container = document.createElement('div');
    container.innerHTML = html;

    var INLINE_TAGS = ['A','ABBR','B','BDO','BR','CITE','CODE','DFN','EM','I',
        'IMG','KBD','MARK','Q','S','SAMP','SMALL','SPAN','STRONG','SUB','SUP',
        'TIME','U','VAR','WBR'];

    function isInline(node) {
        if (node.nodeType !== 1) return true;
        return INLINE_TAGS.indexOf(node.tagName) !== -1;
    }

    function unwrap(el) {
        var parent = el.parentNode;
        while (el.firstChild) parent.insertBefore(el.firstChild, el);
        parent.removeChild(el);
    }

    function clean(root) {
        var children = Array.from(root.childNodes);
        for (var i = 0; i < children.length; i++) {
            if (children[i].nodeType === 1) clean(children[i]);
        }
        if (root.nodeType !== 1) return;
        var tag = root.tagName;

        if (tag === 'SPAN' && root.attributes.length === 0) {
            unwrap(root);
            return;
        }

        if (tag === 'DIV' && root.attributes.length === 0) {
            var kids = Array.from(root.childNodes).filter(function(n) {
                return !(n.nodeType === 3 && n.textContent.trim() === '');
            });
            if (kids.length === 1 && kids[0].nodeType === 1 && kids[0].tagName === 'DIV') {
                unwrap(root);
                return;
            }
            if (kids.length > 0 && kids.every(isInline)) {
                unwrap(root);
                return;
            }
        }
    }

    Array.from(container.children).forEach(clean);
    return container.innerHTML;
})
"#;

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
    tab
}

/// Evaluate the cleaning function as a single self-contained IIFE.
fn clean(tab: &headless_chrome::Tab, html: &str) -> String {
    let escaped = serde_json::to_string(html).unwrap();
    let js = format!("{CLEAN_JS}({escaped})");
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
fn span_with_class_kept() {
    let tab = new_tab();
    let input = r#"<span class="highlight">hello</span>"#;
    assert_eq!(clean(&tab, input), input);
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
fn div_with_class_kept() {
    let tab = new_tab();
    let input = r#"<div class="container"><p>text</p></div>"#;
    assert_eq!(clean(&tab, input), input);
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
