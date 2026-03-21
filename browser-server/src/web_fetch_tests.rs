use crate::web_fetch::{AXNode, AXProperty, AXValue, format_ax_tree, validate_url};

/// Helper to build an AXNode with common defaults.
fn make_node(
    node_id: &str,
    role: &str,
    name: &str,
    parent_id: Option<&str>,
    child_ids: Vec<&str>,
) -> AXNode {
    AXNode {
        node_id: node_id.to_string(),
        ignored: false,
        role: if role.is_empty() {
            None
        } else {
            Some(AXValue {
                value: Some(serde_json::Value::String(role.to_string())),
            })
        },
        name: if name.is_empty() {
            None
        } else {
            Some(AXValue {
                value: Some(serde_json::Value::String(name.to_string())),
            })
        },
        description: None,
        value: None,
        properties: None,
        parent_id: parent_id.map(|s| s.to_string()),
        child_ids: if child_ids.is_empty() {
            None
        } else {
            Some(child_ids.into_iter().map(|s| s.to_string()).collect())
        },
    }
}

fn make_node_with_props(
    node_id: &str,
    role: &str,
    name: &str,
    parent_id: Option<&str>,
    child_ids: Vec<&str>,
    properties: Vec<AXProperty>,
) -> AXNode {
    let mut node = make_node(node_id, role, name, parent_id, child_ids);
    node.properties = Some(properties);
    node
}

fn make_prop(name: &str, val: &str) -> AXProperty {
    AXProperty {
        name: name.to_string(),
        value: AXValue {
            value: Some(serde_json::Value::String(val.to_string())),
        },
    }
}

fn make_int_prop(name: &str, val: i64) -> AXProperty {
    AXProperty {
        name: name.to_string(),
        value: AXValue {
            value: Some(serde_json::Value::Number(serde_json::Number::from(val))),
        },
    }
}

// --- format_ax_tree tests ---

#[test]
fn empty_tree() {
    assert_eq!(format_ax_tree(&[]), "");
}

#[test]
fn single_static_text() {
    let nodes = vec![make_node("1", "StaticText", "Hello world", None, vec![])];
    assert_eq!(format_ax_tree(&nodes), "Hello world\n");
}

#[test]
fn heading_with_level() {
    let nodes = vec![
        make_node_with_props(
            "1",
            "heading",
            "Welcome",
            None,
            vec!["2"],
            vec![make_int_prop("level", 1)],
        ),
        make_node("2", "StaticText", "Welcome", Some("1"), vec![]),
    ];
    let result = format_ax_tree(&nodes);
    assert!(
        result.contains("heading \"Welcome\" level: 1"),
        "got: {result}"
    );
}

#[test]
fn link_with_url() {
    let nodes = vec![
        make_node_with_props(
            "1",
            "link",
            "Home",
            None,
            vec!["2"],
            vec![make_prop("url", "https://example.com/")],
        ),
        make_node("2", "StaticText", "Home", Some("1"), vec![]),
    ];
    let result = format_ax_tree(&nodes);
    assert!(
        result.contains("link \"Home\" url: https://example.com/"),
        "got: {result}"
    );
}

#[test]
fn ignored_nodes_skipped() {
    let mut ignored = make_node("1", "generic", "", None, vec!["2"]);
    ignored.ignored = true;
    let nodes = vec![
        ignored,
        make_node("2", "StaticText", "visible text", Some("1"), vec![]),
    ];
    let result = format_ax_tree(&nodes);
    assert_eq!(result, "visible text\n");
}

#[test]
fn generic_role_transparent() {
    // generic containers should be skipped, children emitted at the same depth
    let nodes = vec![
        make_node("1", "generic", "", None, vec!["2"]),
        make_node("2", "StaticText", "inside generic", Some("1"), vec![]),
    ];
    let result = format_ax_tree(&nodes);
    // The text should appear at depth 0 (no indentation)
    assert_eq!(result, "inside generic\n");
}

#[test]
fn nested_structure() {
    let nodes = vec![
        make_node("1", "navigation", "Main Nav", None, vec!["2", "3"]),
        make_node_with_props(
            "2",
            "link",
            "Home",
            Some("1"),
            vec![],
            vec![make_prop("url", "/")],
        ),
        make_node_with_props(
            "3",
            "link",
            "About",
            Some("1"),
            vec![],
            vec![make_prop("url", "/about")],
        ),
    ];
    let result = format_ax_tree(&nodes);
    assert!(result.contains("navigation \"Main Nav\""), "got: {result}");
    assert!(result.contains("  link \"Home\" url: /"), "got: {result}");
    assert!(
        result.contains("  link \"About\" url: /about"),
        "got: {result}"
    );
}

#[test]
fn node_with_value() {
    let mut node = make_node("1", "textbox", "Search", None, vec![]);
    node.value = Some(AXValue {
        value: Some(serde_json::Value::String("query text".to_string())),
    });
    let result = format_ax_tree(&[node]);
    assert!(result.contains("value: query text"), "got: {result}");
}

#[test]
fn checkbox_with_checked() {
    let nodes = vec![make_node_with_props(
        "1",
        "checkbox",
        "Accept terms",
        None,
        vec![],
        vec![make_prop("checked", "true")],
    )];
    let result = format_ax_tree(&nodes);
    assert!(result.contains("checked: true"), "got: {result}");
}

#[test]
fn paragraph_with_mixed_content() {
    // paragraph > StaticText + link > StaticText
    let nodes = vec![
        make_node("1", "paragraph", "", None, vec!["2", "3"]),
        make_node("2", "StaticText", "Read the ", Some("1"), vec![]),
        make_node_with_props(
            "3",
            "link",
            "documentation",
            Some("1"),
            vec!["4"],
            vec![make_prop("url", "/docs")],
        ),
        make_node("4", "StaticText", "documentation", Some("3"), vec![]),
    ];
    let result = format_ax_tree(&nodes);
    assert!(result.contains("paragraph"), "got: {result}");
    assert!(result.contains("  Read the "), "got: {result}");
    assert!(
        result.contains("  link \"documentation\" url: /docs"),
        "got: {result}"
    );
}

#[test]
fn unknown_property_tolerated() {
    // Ensure properties like "uninteresting" don't cause failures
    let nodes = vec![make_node_with_props(
        "1",
        "paragraph",
        "text",
        None,
        vec![],
        vec![
            make_prop("uninteresting", "true"),
            make_prop("url", "http://x"),
        ],
    )];
    let result = format_ax_tree(&nodes);
    assert!(result.contains("url: http://x"), "got: {result}");
}

// --- URL validation tests ---

#[test]
fn validate_url_allows_https() {
    assert!(validate_url("https://example.com").is_ok());
}

#[test]
fn validate_url_allows_http_with_path() {
    assert!(validate_url("http://example.com/path?q=1").is_ok());
}

#[test]
fn validate_url_blocks_file_scheme() {
    let err = validate_url("file:///etc/passwd").unwrap_err();
    assert!(err.to_string().contains("Blocked URL scheme"), "{err}");
}

#[test]
fn validate_url_blocks_chrome_scheme() {
    let err = validate_url("chrome://settings").unwrap_err();
    assert!(err.to_string().contains("Blocked URL scheme"), "{err}");
}

#[test]
fn validate_url_blocks_data_scheme() {
    let err = validate_url("data:text/html,<h1>hi</h1>").unwrap_err();
    assert!(err.to_string().contains("Blocked URL scheme"), "{err}");
}

#[test]
fn validate_url_blocks_javascript_scheme() {
    let err = validate_url("javascript:alert(1)").unwrap_err();
    assert!(err.to_string().contains("Blocked URL scheme"), "{err}");
}

#[test]
fn validate_url_blocks_localhost() {
    let err = validate_url("http://localhost").unwrap_err();
    assert!(err.to_string().contains("Blocked host"), "{err}");
}

#[test]
fn validate_url_blocks_localhost_with_port() {
    let err = validate_url("http://localhost:8080").unwrap_err();
    assert!(err.to_string().contains("Blocked host"), "{err}");
}

#[test]
fn validate_url_blocks_127_0_0_1() {
    let err = validate_url("http://127.0.0.1").unwrap_err();
    assert!(
        err.to_string().contains("Blocked private/internal IP"),
        "{err}"
    );
}

#[test]
fn validate_url_blocks_ipv6_loopback() {
    let err = validate_url("http://[::1]").unwrap_err();
    assert!(
        err.to_string().contains("Blocked private/internal IP"),
        "{err}"
    );
}

#[test]
fn validate_url_blocks_link_local() {
    let err = validate_url("http://169.254.169.254").unwrap_err();
    assert!(
        err.to_string().contains("Blocked private/internal IP"),
        "{err}"
    );
}

#[test]
fn validate_url_blocks_10_network() {
    let err = validate_url("http://10.0.0.1").unwrap_err();
    assert!(
        err.to_string().contains("Blocked private/internal IP"),
        "{err}"
    );
}

#[test]
fn validate_url_blocks_192_168_network() {
    let err = validate_url("http://192.168.1.1").unwrap_err();
    assert!(
        err.to_string().contains("Blocked private/internal IP"),
        "{err}"
    );
}

#[test]
fn validate_url_blocks_172_16_network() {
    let err = validate_url("http://172.16.0.1").unwrap_err();
    assert!(
        err.to_string().contains("Blocked private/internal IP"),
        "{err}"
    );
}

#[test]
fn validate_url_blocks_unspecified() {
    let err = validate_url("http://0.0.0.0").unwrap_err();
    assert!(
        err.to_string().contains("Blocked private/internal IP"),
        "{err}"
    );
}

#[test]
fn validate_url_blocks_cgnat() {
    let err = validate_url("http://100.64.0.1").unwrap_err();
    assert!(
        err.to_string().contains("Blocked private/internal IP"),
        "{err}"
    );
}
