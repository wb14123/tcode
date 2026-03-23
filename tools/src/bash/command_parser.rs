use std::path::PathBuf;

/// Classification of a parsed bash command for permission routing.
#[derive(Debug)]
pub enum CommandClassification {
    /// A command from the read whitelist (cat, head, tail, etc.)
    ReadCommand { paths: Vec<PathBuf> },
    /// A command from the write whitelist (mkdir, touch)
    WriteCommand { paths: Vec<PathBuf> },
    /// A simple command not in any whitelist
    OtherSimple { tokens: Vec<String> },
    /// A complex command (pipeline, compound, substitution, etc.)
    Complex,
}

/// Redirections extracted from a command.
#[derive(Debug, Default)]
pub struct Redirections {
    /// Files used as input via `<`
    pub input_files: Vec<PathBuf>,
    /// Files used as output via `>` or `>>`
    pub output_files: Vec<PathBuf>,
}

/// Result of parsing a bash command.
#[derive(Debug)]
pub struct ParsedCommand {
    pub classification: CommandClassification,
    pub redirections: Redirections,
}

/// Commands that only read files and never modify anything.
const READ_WHITELIST: &[&str] = &[
    "cat",
    "head",
    "tail",
    "less",
    "more",
    "bat",
    "wc",
    "diff",
    "cmp",
    "comm",
    "md5sum",
    "sha256sum",
    "sha1sum",
    "cksum",
    "file",
    "stat",
    "readlink",
    "realpath",
    "strings",
    "xxd",
    "od",
    "hexdump",
    "nl",
    "tac",
    "rev",
];

/// Commands that create files or directories (constructive writes only).
const WRITE_WHITELIST: &[&str] = &["mkdir", "touch"];

/// Parse a bash command string and classify it for permission routing.
pub fn parse_command(command: &str) -> ParsedCommand {
    let mut parser = tree_sitter::Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    parser
        .set_language(&language.into())
        .expect("tree-sitter-bash language should load");

    let tree = match parser.parse(command, None) {
        Some(t) => t,
        None => {
            return ParsedCommand {
                classification: CommandClassification::Complex,
                redirections: Redirections::default(),
            };
        }
    };

    let root = tree.root_node();

    // Check if the tree has errors (parse failures) — treat as complex
    if root.has_error() {
        return ParsedCommand {
            classification: CommandClassification::Complex,
            redirections: Redirections::default(),
        };
    }

    let source = command.as_bytes();

    // Check for complex constructs: the root program node should contain exactly
    // one child that is a simple_command (possibly wrapped in a redirected_statement).
    // Anything else (pipeline, list, compound_statement, subshell, etc.) is complex.
    let program_children: Vec<_> = named_children(&root);

    if program_children.len() != 1 {
        return ParsedCommand {
            classification: CommandClassification::Complex,
            redirections: Redirections::default(),
        };
    }

    let top_node = program_children[0];

    // Unwrap redirected_statement to get at the inner command + redirections
    let (cmd_node, redirect_node) = if top_node.kind() == "redirected_statement" {
        let inner = named_children(&top_node);
        let cmd = inner.iter().find(|n| n.kind() != "file_redirect");
        let redirect_parent = top_node;
        match cmd {
            Some(c) => (*c, Some(redirect_parent)),
            None => {
                return ParsedCommand {
                    classification: CommandClassification::Complex,
                    redirections: Redirections::default(),
                };
            }
        }
    } else {
        (top_node, None)
    };

    // Must be a simple_command — anything else (pipeline, list, subshell,
    // command_substitution at top level, etc.) is complex
    if cmd_node.kind() != "command" {
        return ParsedCommand {
            classification: CommandClassification::Complex,
            redirections: Redirections::default(),
        };
    }

    // Check for variable expansion in command position or any complex nested constructs
    if has_complex_descendants(&cmd_node) {
        return ParsedCommand {
            classification: CommandClassification::Complex,
            redirections: Redirections::default(),
        };
    }

    // Extract command name and arguments
    let mut cmd_name: Option<String> = None;
    let mut args: Vec<String> = Vec::new();

    for child in named_children(&cmd_node) {
        let text = node_text(&child, source);
        match child.kind() {
            "command_name" => {
                // command_name may contain a "word" child — use text of whole node
                cmd_name = Some(text);
            }
            "word" | "concatenation" | "string" | "raw_string" | "number" => {
                args.push(unquote(&text));
            }
            // Variable expansion, command substitution etc. in arguments → complex
            "expansion" | "simple_expansion" | "command_substitution" | "process_substitution" => {
                return ParsedCommand {
                    classification: CommandClassification::Complex,
                    redirections: Redirections::default(),
                };
            }
            _ => {
                args.push(unquote(&text));
            }
        }
    }

    // Extract redirections
    let redirections = extract_redirections(redirect_node, &cmd_node, source);

    let cmd_name = match cmd_name {
        Some(name) => name,
        None => {
            return ParsedCommand {
                classification: CommandClassification::Complex,
                redirections,
            };
        }
    };

    // Classify based on whitelists
    let classification = if READ_WHITELIST.contains(&cmd_name.as_str()) {
        let paths = extract_paths_from_args(&args);
        CommandClassification::ReadCommand { paths }
    } else if WRITE_WHITELIST.contains(&cmd_name.as_str()) {
        let paths = extract_paths_from_args(&args);
        CommandClassification::WriteCommand { paths }
    } else {
        let mut tokens = vec![cmd_name];
        tokens.extend(args);
        CommandClassification::OtherSimple { tokens }
    };

    ParsedCommand {
        classification,
        redirections,
    }
}

/// Collect named (non-anonymous) children of a tree-sitter node.
fn named_children<'a>(node: &tree_sitter::Node<'a>) -> Vec<tree_sitter::Node<'a>> {
    let mut children = Vec::new();
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            if child.is_named() {
                children.push(child);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    children
}

/// Get the text of a tree-sitter node from source bytes.
fn node_text(node: &tree_sitter::Node, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").to_string()
}

/// Check if any descendant contains a complex construct that makes
/// static analysis unreliable.
fn has_complex_descendants(node: &tree_sitter::Node) -> bool {
    let mut cursor = node.walk();
    let mut stack = vec![*node];

    while let Some(current) = stack.pop() {
        match current.kind() {
            "command_substitution"
            | "process_substitution"
            | "subshell"
            | "expansion"
            | "simple_expansion" => {
                // Only flag if it's not the root node itself
                if current.id() != node.id() {
                    return true;
                }
            }
            _ => {}
        }

        cursor.reset(current);
        if cursor.goto_first_child() {
            loop {
                stack.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    false
}

/// Extract redirections from a redirected_statement and from any file_redirect
/// children of the command node itself.
fn extract_redirections(
    redirect_node: Option<tree_sitter::Node>,
    cmd_node: &tree_sitter::Node,
    source: &[u8],
) -> Redirections {
    let mut redirections = Redirections::default();

    let nodes_to_check: Vec<tree_sitter::Node> = if let Some(rn) = redirect_node {
        named_children(&rn)
            .into_iter()
            .chain(named_children(cmd_node))
            .collect()
    } else {
        named_children(cmd_node)
    };

    for child in nodes_to_check {
        if child.kind() == "file_redirect" {
            let text = node_text(&child, source);

            // Skip fd-to-fd redirects like `2>&1`, `>&2`, `&>` — these
            // redirect between file descriptors, not to/from files.
            if text.contains(">&") || text.contains("&>") {
                continue;
            }

            // Determine direction from the redirect operator
            let children = named_children(&child);
            let dest = children.last().map(|n| unquote(&node_text(n, source)));

            if let Some(path_str) = dest {
                let path = PathBuf::from(path_str);
                if text.starts_with(">>") || text.starts_with('>') {
                    redirections.output_files.push(path);
                } else if text.starts_with('<') {
                    redirections.input_files.push(path);
                } else {
                    // Check for the operator token in anonymous children
                    let mut cursor = child.walk();
                    if cursor.goto_first_child() {
                        loop {
                            let n = cursor.node();
                            if !n.is_named() {
                                let op = node_text(&n, source);
                                if op.contains(">>") || op.contains('>') {
                                    redirections.output_files.push(path.clone());
                                    break;
                                } else if op.contains('<') {
                                    redirections.input_files.push(path.clone());
                                    break;
                                }
                            }
                            if !cursor.goto_next_sibling() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    redirections
}

/// Remove surrounding quotes from a string.
fn unquote(s: &str) -> String {
    let s = s.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(quote).and_then(|s| s.strip_suffix(quote)) {
            return inner.to_string();
        }
    }
    s.to_string()
}

/// Extract file paths from command arguments, skipping flags.
///
/// Flags (arguments starting with `-`) are skipped. Non-flag arguments
/// are included only if they pass the `looks_like_path` heuristic,
/// which naturally filters out flag values like numbers.
pub fn extract_paths_from_args(args: &[String]) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for arg in args {
        if arg.starts_with('-') {
            continue;
        }
        if looks_like_path(arg) {
            paths.push(PathBuf::from(arg));
        }
    }
    paths
}

/// Heuristic to determine if an argument looks like a file path.
pub(crate) fn looks_like_path(arg: &str) -> bool {
    arg.starts_with('/') || arg.starts_with("./") || arg.starts_with("../") || arg.contains('/')
}
