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

fn setup() -> (Browser, std::sync::Arc<headless_chrome::Tab>) {
    let browser = Browser::new(LaunchOptions {
        headless: true,
        ..LaunchOptions::default()
    })
    .expect("failed to launch browser");
    let tab = browser.new_tab().expect("failed to open tab");
    tab.navigate_to("about:blank").unwrap();
    tab.wait_until_navigated().unwrap();
    (browser, tab)
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
    let (_b, tab) = setup();
    assert_eq!(clean(&tab, "<span>hello</span>"), "hello");
}

#[test]
fn span_with_class_kept() {
    let (_b, tab) = setup();
    let input = r#"<span class="highlight">hello</span>"#;
    assert_eq!(clean(&tab, input), input);
}

#[test]
fn nested_spans_unwrapped() {
    let (_b, tab) = setup();
    assert_eq!(clean(&tab, "<span><span>deep</span></span>"), "deep");
}

#[test]
fn div_wrapping_single_div_flattened() {
    let (_b, tab) = setup();
    let input = "<div><div><p>text</p></div></div>";
    assert_eq!(clean(&tab, input), "<div><p>text</p></div>");
}

#[test]
fn deeply_nested_divs_flattened() {
    let (_b, tab) = setup();
    let input = "<div><div><div><p>text</p></div></div></div>";
    assert_eq!(clean(&tab, input), "<div><p>text</p></div>");
}

#[test]
fn div_with_inline_content_unwrapped() {
    let (_b, tab) = setup();
    assert_eq!(clean(&tab, "<div>just text</div>"), "just text");
}

#[test]
fn div_with_inline_elements_unwrapped() {
    let (_b, tab) = setup();
    assert_eq!(
        clean(&tab, "<div><strong>bold</strong> and <em>italic</em></div>"),
        "<strong>bold</strong> and <em>italic</em>"
    );
}

#[test]
fn div_with_block_children_kept() {
    let (_b, tab) = setup();
    let input = "<div><p>one</p><p>two</p></div>";
    assert_eq!(clean(&tab, input), input);
}

#[test]
fn div_with_class_kept() {
    let (_b, tab) = setup();
    let input = r#"<div class="container"><p>text</p></div>"#;
    assert_eq!(clean(&tab, input), input);
}

#[test]
fn semantic_elements_preserved() {
    let (_b, tab) = setup();
    let input = "<h1>Title</h1><ul><li>item</li></ul><pre><code>x</code></pre>";
    assert_eq!(clean(&tab, input), input);
}

#[test]
fn links_preserved() {
    let (_b, tab) = setup();
    let input = r#"<a href="https://example.com">link</a>"#;
    assert_eq!(clean(&tab, input), input);
}

#[test]
fn mixed_real_world_cleanup() {
    let (_b, tab) = setup();
    let input = "<div><div><div><span>Hello</span> <span>world</span></div></div><p>Paragraph</p></div>";
    let result = clean(&tab, input);
    assert!(!result.contains("<span>"), "bare spans should be removed");
    assert!(result.contains("<p>Paragraph</p>"), "p should be preserved");
    assert!(result.contains("Hello"), "text content preserved");
}
