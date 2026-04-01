#![allow(deprecated)] // DocumentSymbol::deprecated field

use std::path::Path;

use lsp_types::*;

use super::formatters;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_uri(path: &str) -> Uri {
    format!("file://{path}").parse().unwrap()
}

fn test_range(line: u32, col: u32, end_col: u32) -> Range {
    Range {
        start: Position {
            line,
            character: col,
        },
        end: Position {
            line,
            character: end_col,
        },
    }
}

fn test_location(path: &str, line: u32, col: u32) -> Location {
    Location {
        uri: test_uri(path),
        range: test_range(line, col, col + 5),
    }
}

/// A root that never matches any test URI so `strip_prefix` falls back to the
/// absolute path. Using `/test` means `file:///test/src/main.rs` becomes
/// `src/main.rs` after stripping.
fn root() -> &'static Path {
    Path::new("/test")
}

fn make_call_hierarchy_item(
    name: &str,
    path: &str,
    line: u32,
    kind: SymbolKind,
) -> CallHierarchyItem {
    CallHierarchyItem {
        name: name.to_string(),
        kind,
        tags: None,
        detail: None,
        uri: test_uri(path),
        range: test_range(line, 0, 10),
        selection_range: test_range(line, 0, 5),
        data: None,
    }
}

// ===========================================================================
// format_definition
// ===========================================================================

#[test]
fn definition_scalar_single_location() {
    let loc = test_location("/test/src/main.rs", 9, 4);
    let response = GotoDefinitionResponse::Scalar(loc);
    let out = formatters::format_definition(response, root());
    assert!(out.starts_with("Definition: "));
    assert!(out.contains("src/main.rs:10:5"));
}

#[test]
fn definition_array_multiple_locations() {
    let locs = vec![
        test_location("/test/src/main.rs", 9, 4),
        test_location("/test/src/lib.rs", 19, 0),
    ];
    let response = GotoDefinitionResponse::Array(locs);
    let out = formatters::format_definition(response, root());
    assert!(out.starts_with("Definitions:\n"));
    assert!(out.contains("src/main.rs:10:5"));
    assert!(out.contains("src/lib.rs:20:1"));
}

#[test]
fn definition_array_empty() {
    let response = GotoDefinitionResponse::Array(vec![]);
    let out = formatters::format_definition(response, root());
    assert_eq!(out, "No definitions found.");
}

#[test]
fn definition_link_variant() {
    let links = vec![LocationLink {
        origin_selection_range: None,
        target_uri: test_uri("/test/src/util.rs"),
        target_range: test_range(5, 0, 20),
        target_selection_range: test_range(5, 4, 10),
    }];
    let response = GotoDefinitionResponse::Link(links);
    let out = formatters::format_definition(response, root());
    assert!(out.starts_with("Definitions:\n"));
    // target_selection_range line=5 => displayed as 6, char=4 => 5
    assert!(out.contains("src/util.rs:6:5"));
}

#[test]
fn definition_link_empty() {
    let response = GotoDefinitionResponse::Link(vec![]);
    let out = formatters::format_definition(response, root());
    assert_eq!(out, "No definitions found.");
}

// ===========================================================================
// format_references
// ===========================================================================

#[test]
fn references_multiple_files_grouped() {
    let locations = vec![
        test_location("/test/src/main.rs", 9, 0),
        test_location("/test/src/main.rs", 19, 0),
        test_location("/test/src/lib.rs", 4, 0),
    ];
    let out = formatters::format_references(locations, root());
    assert!(out.starts_with("Found 3 references:\n"));
    // BTreeMap groups and sorts by file name
    assert!(out.contains("src/lib.rs:\n"));
    assert!(out.contains("src/main.rs:\n"));
    assert!(out.contains("  Line 10\n"));
    assert!(out.contains("  Line 20\n"));
    assert!(out.contains("  Line 5\n"));
}

#[test]
fn references_single() {
    let locations = vec![test_location("/test/src/main.rs", 0, 0)];
    let out = formatters::format_references(locations, root());
    assert!(out.starts_with("Found 1 reference:\n"));
    assert!(out.contains("src/main.rs:\n"));
    assert!(out.contains("  Line 1\n"));
}

#[test]
fn references_empty() {
    let out = formatters::format_references(vec![], root());
    assert_eq!(out, "No references found.");
}

// ===========================================================================
// format_hover
// ===========================================================================

#[test]
fn hover_markup_content() {
    let hover = Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: "```rust\nfn main()\n```".to_string(),
        }),
        range: None,
    };
    let out = formatters::format_hover(hover);
    assert!(out.contains("fn main()"));
}

#[test]
fn hover_marked_string_plain() {
    let hover = Hover {
        contents: HoverContents::Scalar(MarkedString::String("some docs".to_string())),
        range: None,
    };
    let out = formatters::format_hover(hover);
    assert_eq!(out, "some docs");
}

#[test]
fn hover_marked_string_language() {
    let hover = Hover {
        contents: HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
            language: "rust".to_string(),
            value: "fn foo()".to_string(),
        })),
        range: None,
    };
    let out = formatters::format_hover(hover);
    assert!(out.contains("```rust"));
    assert!(out.contains("fn foo()"));
    assert!(out.contains("```"));
}

#[test]
fn hover_array_of_marked_strings() {
    let hover = Hover {
        contents: HoverContents::Array(vec![
            MarkedString::String("Part one".to_string()),
            MarkedString::String("Part two".to_string()),
        ]),
        range: None,
    };
    let out = formatters::format_hover(hover);
    assert!(out.contains("Part one"));
    assert!(out.contains("Part two"));
    // Joined by double newline
    assert!(out.contains("Part one\n\nPart two"));
}

#[test]
fn hover_empty_content() {
    let hover = Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value: "   ".to_string(),
        }),
        range: None,
    };
    let out = formatters::format_hover(hover);
    assert_eq!(out, "No hover information available.");
}

// ===========================================================================
// format_document_symbols
// ===========================================================================

#[test]
fn document_symbols_nested_with_children() {
    let child = DocumentSymbol {
        name: "bar".to_string(),
        detail: None,
        kind: SymbolKind::FIELD,
        tags: None,
        deprecated: None,
        range: test_range(5, 0, 20),
        selection_range: test_range(5, 4, 7),
        children: None,
    };
    let parent = DocumentSymbol {
        name: "Foo".to_string(),
        detail: None,
        kind: SymbolKind::STRUCT,
        tags: None,
        deprecated: None,
        range: test_range(3, 0, 30),
        selection_range: test_range(3, 4, 7),
        children: Some(vec![child]),
    };
    let response = DocumentSymbolResponse::Nested(vec![parent]);
    let out = formatters::format_document_symbols(response, "src/main.rs");
    assert!(out.starts_with("Symbols in src/main.rs:\n"));
    // Parent at depth 1 => 2 spaces
    assert!(out.contains("  [Struct] Foo (line 4)\n"));
    // Child at depth 2 => 4 spaces
    assert!(out.contains("    [Field] bar (line 6)\n"));
}

#[test]
fn document_symbols_flat() {
    #[allow(deprecated)]
    let sym = SymbolInformation {
        name: "main".to_string(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        deprecated: None,
        location: test_location("/test/src/main.rs", 0, 0),
        container_name: None,
    };
    let response = DocumentSymbolResponse::Flat(vec![sym]);
    let out = formatters::format_document_symbols(response, "src/main.rs");
    assert!(out.starts_with("Symbols in src/main.rs:\n"));
    assert!(out.contains("  [Function] main (line 1)\n"));
}

#[test]
fn document_symbols_empty_nested() {
    let response = DocumentSymbolResponse::Nested(vec![]);
    let out = formatters::format_document_symbols(response, "src/main.rs");
    assert_eq!(out, "No symbols found in src/main.rs.");
}

#[test]
fn document_symbols_empty_flat() {
    let response = DocumentSymbolResponse::Flat(vec![]);
    let out = formatters::format_document_symbols(response, "src/main.rs");
    assert_eq!(out, "No symbols found in src/main.rs.");
}

#[test]
fn document_symbols_various_kinds() {
    let symbols = vec![
        DocumentSymbol {
            name: "MyStruct".to_string(),
            detail: None,
            kind: SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            range: test_range(0, 0, 10),
            selection_range: test_range(0, 0, 8),
            children: None,
        },
        DocumentSymbol {
            name: "my_func".to_string(),
            detail: None,
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: test_range(10, 0, 10),
            selection_range: test_range(10, 0, 7),
            children: None,
        },
        DocumentSymbol {
            name: "MY_CONST".to_string(),
            detail: None,
            kind: SymbolKind::CONSTANT,
            tags: None,
            deprecated: None,
            range: test_range(20, 0, 10),
            selection_range: test_range(20, 0, 8),
            children: None,
        },
    ];
    let response = DocumentSymbolResponse::Nested(symbols);
    let out = formatters::format_document_symbols(response, "src/lib.rs");
    assert!(out.contains("[Struct] MyStruct"));
    assert!(out.contains("[Function] my_func"));
    assert!(out.contains("[Constant] MY_CONST"));
}

// ===========================================================================
// format_workspace_symbols
// ===========================================================================

#[test]
fn workspace_symbols_flat_grouped_by_file() {
    #[allow(deprecated)]
    let symbols = vec![
        SymbolInformation {
            name: "foo".to_string(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            location: test_location("/test/src/main.rs", 0, 0),
            container_name: None,
        },
        SymbolInformation {
            name: "Bar".to_string(),
            kind: SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            location: test_location("/test/src/lib.rs", 4, 0),
            container_name: None,
        },
        SymbolInformation {
            name: "baz".to_string(),
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            location: test_location("/test/src/main.rs", 9, 0),
            container_name: None,
        },
    ];
    let response = WorkspaceSymbolResponse::Flat(symbols);
    let out = formatters::format_workspace_symbols(response, root());
    assert!(out.starts_with("Found 3 symbols:\n"));
    // BTreeMap sorts files
    assert!(out.contains("src/lib.rs:\n"));
    assert!(out.contains("src/main.rs:\n"));
    assert!(out.contains("[Function] foo (line 1)"));
    assert!(out.contains("[Struct] Bar (line 5)"));
    assert!(out.contains("[Function] baz (line 10)"));
}

#[test]
fn workspace_symbols_flat_empty() {
    let response = WorkspaceSymbolResponse::Flat(vec![]);
    let out = formatters::format_workspace_symbols(response, root());
    assert_eq!(out, "No symbols found.");
}

#[test]
fn workspace_symbols_nested_with_location() {
    let symbols = vec![WorkspaceSymbol {
        name: "MyTrait".to_string(),
        kind: SymbolKind::INTERFACE,
        tags: None,
        container_name: None,
        location: OneOf::Left(test_location("/test/src/traits.rs", 2, 0)),
        data: None,
    }];
    let response = WorkspaceSymbolResponse::Nested(symbols);
    let out = formatters::format_workspace_symbols(response, root());
    assert!(out.starts_with("Found 1 symbol:\n"));
    assert!(out.contains("[Interface] MyTrait (src/traits.rs:3)"));
}

#[test]
fn workspace_symbols_nested_empty() {
    let response = WorkspaceSymbolResponse::Nested(vec![]);
    let out = formatters::format_workspace_symbols(response, root());
    assert_eq!(out, "No symbols found.");
}

// ===========================================================================
// format_call_hierarchy_item
// ===========================================================================

#[test]
fn call_hierarchy_item_formatting() {
    let item = make_call_hierarchy_item("do_thing", "/test/src/main.rs", 14, SymbolKind::FUNCTION);
    let out = formatters::format_call_hierarchy_item(&item, root());
    assert_eq!(out, "[Function] do_thing (src/main.rs:15)");
}

#[test]
fn call_hierarchy_item_method() {
    let item = make_call_hierarchy_item("run", "/test/src/server.rs", 99, SymbolKind::METHOD);
    let out = formatters::format_call_hierarchy_item(&item, root());
    assert_eq!(out, "[Method] run (src/server.rs:100)");
}

// ===========================================================================
// format_incoming_calls
// ===========================================================================

#[test]
fn incoming_calls_multiple() {
    let calls = vec![
        CallHierarchyIncomingCall {
            from: make_call_hierarchy_item(
                "caller_a",
                "/test/src/main.rs",
                10,
                SymbolKind::FUNCTION,
            ),
            from_ranges: vec![test_range(12, 4, 15)],
        },
        CallHierarchyIncomingCall {
            from: make_call_hierarchy_item("caller_b", "/test/src/lib.rs", 20, SymbolKind::METHOD),
            from_ranges: vec![test_range(22, 8, 20), test_range(30, 2, 10)],
        },
    ];
    let out = formatters::format_incoming_calls(calls, root());
    assert!(out.starts_with("Found 2 incoming calls:\n"));
    assert!(out.contains("[Function] caller_a (src/main.rs:11)"));
    assert!(out.contains("    at line 13:5\n"));
    assert!(out.contains("[Method] caller_b (src/lib.rs:21)"));
    assert!(out.contains("    at line 23:9\n"));
    assert!(out.contains("    at line 31:3\n"));
}

#[test]
fn incoming_calls_empty() {
    let out = formatters::format_incoming_calls(vec![], root());
    assert_eq!(out, "No incoming calls found.");
}

// ===========================================================================
// format_outgoing_calls
// ===========================================================================

#[test]
fn outgoing_calls_multiple() {
    let calls = vec![
        CallHierarchyOutgoingCall {
            to: make_call_hierarchy_item("target_x", "/test/src/util.rs", 5, SymbolKind::FUNCTION),
            from_ranges: vec![test_range(15, 8, 20)],
        },
        CallHierarchyOutgoingCall {
            to: make_call_hierarchy_item("target_y", "/test/src/util.rs", 30, SymbolKind::FUNCTION),
            from_ranges: vec![test_range(16, 8, 20)],
        },
    ];
    let out = formatters::format_outgoing_calls(calls, root());
    assert!(out.starts_with("Found 2 outgoing calls:\n"));
    assert!(out.contains("[Function] target_x (src/util.rs:6)"));
    assert!(out.contains("    at line 16:9\n"));
    assert!(out.contains("[Function] target_y (src/util.rs:31)"));
    assert!(out.contains("    at line 17:9\n"));
}

#[test]
fn outgoing_calls_empty() {
    let out = formatters::format_outgoing_calls(vec![], root());
    assert_eq!(out, "No outgoing calls found.");
}
