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

/// Result of attempting to decompose a complex command.
#[derive(Debug)]
pub struct DecomposedCommand {
    /// The individual sub-commands extracted from the compound command.
    /// Each is the source text of one pipeline stage or sequential command.
    pub sub_commands: Vec<String>,
    /// Redirections that apply at the compound level (e.g., pipeline-level `> file`).
    pub redirections: Redirections,
}

/// Commands that only read files and never modify anything.
const READ_WHITELIST: &[&str] = &[
    "echo",
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

/// Commands that mutate shell state, making decomposition unsafe for &&/||/; chains.
/// In pipelines, each stage runs in a subshell, so these are only problematic in sequential chains.
pub const STATE_MUTATING_COMMANDS: &[&str] = &[
    "cd", "pushd", "popd", "export", "source", ".", "eval", "exec", "unset", "set", "alias",
    "shopt", "trap", "builtin", "command", "hash", "enable",
];

/// Create a new tree-sitter parser configured for bash.
fn new_bash_parser() -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .expect("tree-sitter-bash language should load");
    parser
}

/// Shorthand for returning a Complex classification with no redirections.
fn complex_fallback() -> ParsedCommand {
    ParsedCommand {
        classification: CommandClassification::Complex,
        redirections: Redirections::default(),
    }
}

/// Parse a bash command string and classify it for permission routing.
pub fn parse_command(command: &str) -> ParsedCommand {
    let mut parser = new_bash_parser();

    let tree = match parser.parse(command, None) {
        Some(t) => t,
        None => return complex_fallback(),
    };

    let root = tree.root_node();

    // Check if the tree has errors (parse failures) — treat as complex
    if root.has_error() {
        return complex_fallback();
    }

    let source = command.as_bytes();

    // Check for complex constructs: the root program node should contain exactly
    // one child that is a simple_command (possibly wrapped in a redirected_statement).
    // Anything else (pipeline, list, compound_statement, subshell, etc.) is complex.
    let program_children: Vec<_> = named_children(&root);

    if program_children.len() != 1 {
        return complex_fallback();
    }

    let top_node = program_children[0];

    // Unwrap redirected_statement to get at the inner command + redirections
    let (cmd_node, redirect_node) = if top_node.kind() == "redirected_statement" {
        let inner = named_children(&top_node);
        let cmd = inner.iter().find(|n| n.kind() != "file_redirect");
        let redirect_parent = top_node;
        match cmd {
            Some(c) => (*c, Some(redirect_parent)),
            None => return complex_fallback(),
        }
    } else {
        (top_node, None)
    };

    // Must be a simple_command — anything else (pipeline, list, subshell,
    // command_substitution at top level, etc.) is complex
    if cmd_node.kind() != "command" {
        return complex_fallback();
    }

    // Check for variable expansion in command position or any complex nested constructs
    if has_complex_descendants(&cmd_node) {
        return complex_fallback();
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
                return complex_fallback();
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

/// Attempt to decompose a complex command into individually-checkable sub-commands.
///
/// Returns `Some(DecomposedCommand)` if the command can be broken down into simple
/// parts (each sub-command must parse as non-Complex via `parse_command()`).
/// Returns `None` if decomposition is not possible or not safe.
///
/// Handles:
/// - Pipelines: `cmd1 | cmd2 | cmd3` — each stage extracted
/// - Sequential chains: `cmd1 && cmd2`, `cmd1 || cmd2`, `cmd1; cmd2` — each command extracted
///   (but only if no state-mutating commands like `cd`, `export`, `source`, etc.)
/// - Redirected pipelines: `cmd1 | cmd2 > file` — pipeline stages + top-level redirect
///
/// Does NOT decompose:
/// - Commands with variable expansion, command substitution, subshells
/// - Sequential chains containing state-mutating commands
pub fn try_decompose_complex(command: &str) -> Option<DecomposedCommand> {
    let mut parser = new_bash_parser();

    let tree = parser.parse(command, None)?;
    let root = tree.root_node();

    if root.has_error() {
        return None;
    }

    let program_children: Vec<_> = named_children(&root);

    if program_children.len() == 1 {
        let top_node = program_children[0];

        // Case B: redirected_statement wrapping a pipeline or a list
        if top_node.kind() == "redirected_statement" {
            let inner_children = named_children(&top_node);
            if let Some(pipeline_node) = inner_children.iter().find(|n| n.kind() == "pipeline") {
                // Simple case: redirected_statement directly wraps a pipeline
                let sub_commands = extract_pipeline_stages(*pipeline_node, command)?;
                // file_redirect nodes live on top_node; pipeline_node itself has no file_redirects
                let redirections =
                    extract_redirections(Some(top_node), pipeline_node, command.as_bytes());
                return Some(DecomposedCommand {
                    sub_commands,
                    redirections,
                });
            } else if let Some(list_node) = inner_children.iter().find(|n| n.kind() == "list") {
                // redirected_statement wraps a list (e.g. `echo start && cat file | sort > out`)
                // Decompose the list sequentially, then collect the top-level file redirects
                let mut result = decompose_sequential(command, &[*list_node])?;
                let redir = extract_redirections(Some(top_node), list_node, command.as_bytes());
                result.redirections.input_files.extend(redir.input_files);
                result.redirections.output_files.extend(redir.output_files);
                return Some(result);
            } else {
                return None;
            }
        }

        // Case A: bare pipeline
        if top_node.kind() == "pipeline" {
            let sub_commands = extract_pipeline_stages(top_node, command)?;
            return Some(DecomposedCommand {
                sub_commands,
                redirections: Redirections::default(),
            });
        }

        // Case C: list node (&&/||)
        if top_node.kind() == "list" {
            return decompose_sequential(command, &[top_node]);
        }

        return None;
    }

    // Case D: multiple root children — semicolon-separated commands
    if program_children.len() > 1 {
        return decompose_sequential(command, &program_children);
    }

    None
}

/// Extract the source text of each stage in a `pipeline` node, verifying that none
/// parses as `Complex`.  Returns `None` if any stage is complex.
///
/// A pipeline stage may itself be a `redirected_statement` wrapping a `list`
/// (e.g. `cargo fmt && cargo clippy 2>&1` in `cargo fmt && cargo clippy 2>&1 | tail -n 50`).
/// In that case the list's commands are flattened into the stage list.
fn extract_pipeline_stages(pipeline_node: tree_sitter::Node, command: &str) -> Option<Vec<String>> {
    let stages = named_children(&pipeline_node);
    let mut sub_commands = Vec::new();

    for stage in stages {
        // A stage may be `redirected_statement` wrapping a `list` (e.g. when `&&` precedes `|`
        // in `cmd1 && cmd2 2>&1 | tail`).  Decompose those sequentially.
        if stage.kind() == "redirected_statement" {
            let inner_children = named_children(&stage);
            if let Some(list_node) = inner_children.iter().find(|n| n.kind() == "list") {
                // Recursively decompose the list.  The file_redirect on the
                // redirected_statement is typically a fd-to-fd redirect like `2>&1`
                // (e.g. `cargo fmt && cargo clippy 2>&1 | tail`).  If it happens to
                // be a file redirect (e.g. `&> /tmp/log`), it won't be tracked here.
                // This is conservative — the decomposition fast-path will just not
                // match, and the command falls back to prompting.
                let decomposed = decompose_sequential(command, &[*list_node])?;
                sub_commands.extend(decomposed.sub_commands);
                continue;
            }
        }

        let stage_text = &command[stage.start_byte()..stage.end_byte()];
        let parsed = parse_command(stage_text);
        if matches!(parsed.classification, CommandClassification::Complex) {
            return None;
        }
        sub_commands.push(stage_text.to_string());
    }

    Some(sub_commands)
}

/// Flatten a slice of tree-sitter nodes (which may include nested `list` nodes for
/// `&&`/`||` chains) into individual leaf command nodes.
fn flatten_list_nodes<'a>(
    nodes: &[tree_sitter::Node<'a>],
    leaves: &mut Vec<tree_sitter::Node<'a>>,
) {
    for node in nodes {
        if node.kind() == "list" {
            let children = named_children(node);
            flatten_list_nodes(&children, leaves);
        } else {
            leaves.push(*node);
        }
    }
}

/// Decompose a set of sequential nodes (from a `list` or multiple root children)
/// into sub-commands, rejecting any that are Complex or state-mutating.
///
/// Leaves that are pipelines (or `redirected_statement` wrapping a pipeline) are
/// recursively decomposed into their individual stages, so mixed constructs like
/// `cargo fmt && cargo clippy 2>&1 | tail -n 50` produce the expected flat list
/// of sub-commands.
fn decompose_sequential(command: &str, nodes: &[tree_sitter::Node]) -> Option<DecomposedCommand> {
    let mut leaves = Vec::new();
    flatten_list_nodes(nodes, &mut leaves);

    let mut sub_commands = Vec::new();
    let mut redirections = Redirections::default();

    for leaf in leaves {
        // declaration_command (e.g. `export FOO=bar`) is inherently state-mutating
        if leaf.kind() == "declaration_command" {
            return None;
        }

        match leaf.kind() {
            // Leaf is a pipeline — extract each stage individually
            "pipeline" => {
                let stages = extract_pipeline_stages(leaf, command)?;
                sub_commands.extend(stages);
            }
            // Leaf is a redirected_statement — could wrap a pipeline or a simple command
            "redirected_statement" => {
                let inner_children = named_children(&leaf);
                let inner = inner_children.iter().find(|n| n.kind() != "file_redirect");
                if let Some(inner_node) = inner {
                    if inner_node.kind() == "pipeline" {
                        // Pipeline with top-level redirect (e.g., `cmd1 | cmd2 > file`)
                        let stages = extract_pipeline_stages(*inner_node, command)?;
                        sub_commands.extend(stages);
                        let redir =
                            extract_redirections(Some(leaf), inner_node, command.as_bytes());
                        redirections.input_files.extend(redir.input_files);
                        redirections.output_files.extend(redir.output_files);
                    } else {
                        // Simple command with redirect — treat as a single sub-command
                        let leaf_text = &command[leaf.start_byte()..leaf.end_byte()];
                        let parsed = parse_command(leaf_text);
                        if matches!(parsed.classification, CommandClassification::Complex) {
                            return None;
                        }
                        if is_state_mutating(&parsed) {
                            return None;
                        }
                        sub_commands.push(leaf_text.to_string());
                    }
                } else {
                    return None;
                }
            }
            // Regular leaf — parse and check
            _ => {
                let leaf_text = &command[leaf.start_byte()..leaf.end_byte()];
                let parsed = parse_command(leaf_text);

                if matches!(parsed.classification, CommandClassification::Complex) {
                    return None;
                }

                if is_state_mutating(&parsed) {
                    return None;
                }

                sub_commands.push(leaf_text.to_string());
            }
        }
    }

    Some(DecomposedCommand {
        sub_commands,
        redirections,
    })
}

/// Return `true` if the parsed command's name appears in `STATE_MUTATING_COMMANDS`.
fn is_state_mutating(parsed: &ParsedCommand) -> bool {
    if let CommandClassification::OtherSimple { tokens } = &parsed.classification
        && let Some(cmd_name) = tokens.first()
    {
        return STATE_MUTATING_COMMANDS.contains(&cmd_name.as_str());
    }
    false
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

            // Determine direction from the redirect operator
            let children = named_children(&child);

            // Skip fd-to-fd redirects like `2>&1`, `>&2` — these redirect between
            // file descriptors, not to/from files.  We detect them by checking
            // whether the destination word consists solely of digits (e.g. "1", "2").
            // `&>filename` and `>&filename` are real file redirects and must NOT be
            // skipped.
            if text.contains(">&") || text.contains("&>") {
                let dest_is_fd = children
                    .last()
                    .map(|n| {
                        let t = node_text(n, source);
                        !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
                    })
                    .unwrap_or(false);
                if dest_is_fd {
                    continue;
                }
            }

            let dest = children.last().map(|n| unquote(&node_text(n, source)));

            if let Some(path_str) = dest {
                let path = PathBuf::from(path_str);
                if text.starts_with(">>") || text.starts_with('>') || text.starts_with("&>") {
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
