use std::path::Path;

use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall,
    DocumentSymbolResponse, GotoDefinitionResponse, Hover, HoverContents, Location, MarkedString,
    SymbolKind, WorkspaceSymbolResponse,
};

/// Convert an LSP URI to a path relative to the project root.
fn uri_to_relative_path(uri: &lsp_types::Uri, root: &Path) -> String {
    let uri_str = uri.as_str();
    if let Ok(url) = url::Url::parse(uri_str)
        && let Ok(path) = url.to_file_path()
    {
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.display().to_string();
        }
        return path.display().to_string();
    }
    uri_str.to_string()
}

/// Choose singular or plural form based on count.
fn plural<'a>(count: usize, singular: &'a str, plural_form: &'a str) -> &'a str {
    if count == 1 { singular } else { plural_form }
}

/// Map a SymbolKind to a human-readable name.
fn symbol_kind_name(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::FILE => "File",
        SymbolKind::MODULE => "Module",
        SymbolKind::NAMESPACE => "Namespace",
        SymbolKind::PACKAGE => "Package",
        SymbolKind::CLASS => "Class",
        SymbolKind::METHOD => "Method",
        SymbolKind::PROPERTY => "Property",
        SymbolKind::FIELD => "Field",
        SymbolKind::CONSTRUCTOR => "Constructor",
        SymbolKind::ENUM => "Enum",
        SymbolKind::INTERFACE => "Interface",
        SymbolKind::FUNCTION => "Function",
        SymbolKind::VARIABLE => "Variable",
        SymbolKind::CONSTANT => "Constant",
        SymbolKind::STRING => "String",
        SymbolKind::NUMBER => "Number",
        SymbolKind::BOOLEAN => "Boolean",
        SymbolKind::ARRAY => "Array",
        SymbolKind::OBJECT => "Object",
        SymbolKind::KEY => "Key",
        SymbolKind::NULL => "Null",
        SymbolKind::ENUM_MEMBER => "EnumMember",
        SymbolKind::STRUCT => "Struct",
        SymbolKind::EVENT => "Event",
        SymbolKind::OPERATOR => "Operator",
        SymbolKind::TYPE_PARAMETER => "TypeParameter",
        _ => "Unknown",
    }
}

fn format_location(loc: &Location, root: &Path) -> String {
    let path = uri_to_relative_path(&loc.uri, root);
    let line = loc.range.start.line + 1;
    let col = loc.range.start.character + 1;
    format!("{path}:{line}:{col}")
}

pub fn format_definition(result: GotoDefinitionResponse, root: &Path) -> String {
    match result {
        GotoDefinitionResponse::Scalar(loc) => {
            format!("Definition: {}", format_location(&loc, root))
        }
        GotoDefinitionResponse::Array(locs) => {
            if locs.is_empty() {
                return "No definitions found.".to_string();
            }
            let mut out = String::from("Definitions:\n");
            for loc in &locs {
                out.push_str(&format!("  {}\n", format_location(loc, root)));
            }
            out
        }
        GotoDefinitionResponse::Link(links) => {
            if links.is_empty() {
                return "No definitions found.".to_string();
            }
            let mut out = String::from("Definitions:\n");
            for link in &links {
                let path = uri_to_relative_path(&link.target_uri, root);
                let line = link.target_selection_range.start.line + 1;
                let col = link.target_selection_range.start.character + 1;
                out.push_str(&format!("  {path}:{line}:{col}\n"));
            }
            out
        }
    }
}

pub fn format_references(locations: Vec<Location>, root: &Path) -> String {
    if locations.is_empty() {
        return "No references found.".to_string();
    }

    // Group by file
    let mut by_file: std::collections::BTreeMap<String, Vec<u32>> =
        std::collections::BTreeMap::new();
    for loc in &locations {
        let path = uri_to_relative_path(&loc.uri, root);
        let line = loc.range.start.line + 1;
        by_file.entry(path).or_default().push(line);
    }

    let count = locations.len();
    let mut out = format!(
        "Found {} {}:\n",
        count,
        plural(count, "reference", "references")
    );
    for (file, lines) in &by_file {
        out.push_str(&format!("{file}:\n"));
        for line in lines {
            out.push_str(&format!("  Line {line}\n"));
        }
    }
    out
}

pub fn format_hover(hover: Hover) -> String {
    let text = match hover.contents {
        HoverContents::Scalar(marked) => extract_marked_string(&marked),
        HoverContents::Array(items) => items
            .iter()
            .map(extract_marked_string)
            .collect::<Vec<_>>()
            .join("\n\n"),
        HoverContents::Markup(markup) => markup.value,
    };

    if text.trim().is_empty() {
        "No hover information available.".to_string()
    } else {
        text
    }
}

fn extract_marked_string(marked: &MarkedString) -> String {
    match marked {
        MarkedString::String(s) => s.clone(),
        MarkedString::LanguageString(ls) => {
            format!("```{}\n{}\n```", ls.language, ls.value)
        }
    }
}

pub fn format_document_symbols(response: DocumentSymbolResponse, file_path: &str) -> String {
    match response {
        DocumentSymbolResponse::Flat(symbols) => {
            if symbols.is_empty() {
                return format!("No symbols found in {file_path}.");
            }
            let mut out = format!("Symbols in {file_path}:\n");
            for sym in &symbols {
                let kind = symbol_kind_name(sym.kind);
                let line = sym.location.range.start.line + 1;
                out.push_str(&format!("  [{kind}] {} (line {line})\n", sym.name));
            }
            out
        }
        DocumentSymbolResponse::Nested(symbols) => {
            if symbols.is_empty() {
                return format!("No symbols found in {file_path}.");
            }
            let mut out = format!("Symbols in {file_path}:\n");
            for sym in &symbols {
                format_nested_symbol(&mut out, sym, 1);
            }
            out
        }
    }
}

fn format_nested_symbol(out: &mut String, sym: &lsp_types::DocumentSymbol, depth: usize) {
    let indent = "  ".repeat(depth);
    let kind = symbol_kind_name(sym.kind);
    let line = sym.range.start.line + 1;
    out.push_str(&format!("{indent}[{kind}] {} (line {line})\n", sym.name));
    if let Some(children) = &sym.children {
        for child in children {
            format_nested_symbol(out, child, depth + 1);
        }
    }
}

pub fn format_workspace_symbols(result: WorkspaceSymbolResponse, root: &Path) -> String {
    match result {
        WorkspaceSymbolResponse::Flat(symbols) => {
            if symbols.is_empty() {
                return "No symbols found.".to_string();
            }
            // Group by file
            let mut by_file: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for sym in &symbols {
                let path = uri_to_relative_path(&sym.location.uri, root);
                let kind = symbol_kind_name(sym.kind);
                let line = sym.location.range.start.line + 1;
                by_file
                    .entry(path)
                    .or_default()
                    .push(format!("  [{kind}] {} (line {line})", sym.name));
            }
            let count = symbols.len();
            let mut out = format!("Found {} {}:\n", count, plural(count, "symbol", "symbols"));
            for (file, entries) in &by_file {
                out.push_str(&format!("{file}:\n"));
                for entry in entries {
                    out.push_str(&format!("{entry}\n"));
                }
            }
            out
        }
        WorkspaceSymbolResponse::Nested(symbols) => {
            if symbols.is_empty() {
                return "No symbols found.".to_string();
            }
            let count = symbols.len();
            let mut out = format!("Found {} {}:\n", count, plural(count, "symbol", "symbols"));
            for sym in &symbols {
                let kind = symbol_kind_name(sym.kind);
                let location = match &sym.location {
                    lsp_types::OneOf::Left(loc) => {
                        let path = uri_to_relative_path(&loc.uri, root);
                        let line = loc.range.start.line + 1;
                        format!("{path}:{line}")
                    }
                    lsp_types::OneOf::Right(uri_info) => uri_to_relative_path(&uri_info.uri, root),
                };
                out.push_str(&format!("  [{kind}] {} ({location})\n", sym.name));
            }
            out
        }
    }
}

pub fn format_call_hierarchy_item(item: &CallHierarchyItem, root: &Path) -> String {
    let path = uri_to_relative_path(&item.uri, root);
    let kind = symbol_kind_name(item.kind);
    let line = item.range.start.line + 1;
    format!("[{kind}] {} ({path}:{line})", item.name)
}

pub fn format_incoming_calls(calls: Vec<CallHierarchyIncomingCall>, root: &Path) -> String {
    if calls.is_empty() {
        return "No incoming calls found.".to_string();
    }
    let count = calls.len();
    let mut out = format!(
        "Found {} incoming {}:\n",
        count,
        plural(count, "call", "calls")
    );
    for call in &calls {
        let item = format_call_hierarchy_item(&call.from, root);
        out.push_str(&format!("  {item}\n"));
        for range in &call.from_ranges {
            let line = range.start.line + 1;
            let col = range.start.character + 1;
            out.push_str(&format!("    at line {line}:{col}\n"));
        }
    }
    out
}

pub fn format_outgoing_calls(calls: Vec<CallHierarchyOutgoingCall>, root: &Path) -> String {
    if calls.is_empty() {
        return "No outgoing calls found.".to_string();
    }
    let count = calls.len();
    let mut out = format!(
        "Found {} outgoing {}:\n",
        count,
        plural(count, "call", "calls")
    );
    for call in &calls {
        let item = format_call_hierarchy_item(&call.to, root);
        out.push_str(&format!("  {item}\n"));
        for range in &call.from_ranges {
            let line = range.start.line + 1;
            let col = range.start.character + 1;
            out.push_str(&format!("    at line {line}:{col}\n"));
        }
    }
    out
}
