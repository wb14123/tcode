mod formatters;

#[cfg(test)]
mod formatter_tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lsp_client::LspManager;
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyOutgoingCall,
    CallHierarchyOutgoingCallsParams, DocumentSymbolParams, GotoDefinitionParams,
    GotoDefinitionResponse, HoverParams, Location, Position, ReferenceContext, ReferenceParams,
    TextDocumentIdentifier, TextDocumentPositionParams, WorkspaceSymbolParams,
    WorkspaceSymbolResponse,
};

use llm_rs::permission::ScopedPermissionManager;
use llm_rs::tool::{Tool, ToolContext};

const DESCRIPTION: &str = r#"Interact with Language Server Protocol (LSP) servers to get code intelligence features.

Supported operations:
- goToDefinition: Find where a symbol is defined
- findReferences: Find all references to a symbol
- hover: Get hover information (documentation, type info) for a symbol
- documentSymbol: Get all symbols (functions, classes, variables) in a document
- workspaceSymbol: Search for symbols across the entire workspace (requires 'language' or 'filePath')
- goToImplementation: Find implementations of an interface or abstract method
- prepareCallHierarchy: Get call hierarchy item at a position (functions/methods)
- incomingCalls: Find all functions/methods that call the function at a position
- outgoingCalls: Find all functions/methods called by the function at a position

All position-based operations require:
- filePath: The file to operate on
- line: The line number (1-based, as shown in editors)
- character: The character offset (1-based, as shown in editors)

workspaceSymbol requires either:
- language: The language filetype (e.g. "rust", "python", "typescript") to determine which LSP server to query
- filePath: Any file of the target language (used only for server routing)

The 'language' parameter is ONLY valid for workspaceSymbol. Do not pass it for other operations.

Note: LSP servers are configured from your Neovim LSP setup. If no server is available for a file type, an error will be returned."#;

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LspParams {
    /// The LSP operation to perform
    pub operation: LspOperation,
    /// The file path to operate on
    #[serde(default)]
    pub file_path: Option<String>,
    /// Line number (1-based)
    #[serde(default)]
    pub line: Option<u32>,
    /// Character offset (1-based)
    #[serde(default)]
    pub character: Option<u32>,
    /// Search query (for workspaceSymbol)
    #[serde(default)]
    pub query: Option<String>,
    /// Language filetype for server routing (e.g. "rust", "python", "typescript").
    /// Only used with workspaceSymbol. Uses Neovim filetype names.
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub enum LspOperation {
    #[serde(rename = "goToDefinition")]
    GoToDefinition,
    #[serde(rename = "findReferences")]
    FindReferences,
    #[serde(rename = "hover")]
    Hover,
    #[serde(rename = "documentSymbol")]
    DocumentSymbol,
    #[serde(rename = "workspaceSymbol")]
    WorkspaceSymbol,
    #[serde(rename = "goToImplementation")]
    GoToImplementation,
    #[serde(rename = "prepareCallHierarchy")]
    PrepareCallHierarchy,
    #[serde(rename = "incomingCalls")]
    IncomingCalls,
    #[serde(rename = "outgoingCalls")]
    OutgoingCalls,
}

pub fn lsp_tool(manager: Arc<LspManager>) -> Tool {
    Tool::new::<LspParams, _, _, _, _>(
        "LSP",
        DESCRIPTION,
        Some(Duration::from_millis(30000)),
        move |ctx: ToolContext, params: LspParams| {
            let manager = manager.clone();
            let permission = ctx.permission.clone();
            async_stream::stream! {
                let result = execute_lsp_operation(&manager, &permission, params).await;
                yield result;
            }
        },
    )
}

/// Resolve a file path to an absolute, canonicalized path.
async fn resolve_file_path(file_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(file_path);
    tokio::fs::canonicalize(path)
        .await
        .map_err(|e| format!("Failed to resolve file path '{file_path}': {e}"))
}

/// Get the file extension (with dot, e.g. ".rs") from a path.
fn get_extension(path: &Path) -> Result<String, String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"))
        .ok_or_else(|| format!("Cannot determine file extension for '{}'", path.display()))
}

/// Convert an absolute file path to an lsp_types::Uri.
fn path_to_uri(path: &Path) -> Result<lsp_types::Uri, String> {
    lsp_client::server::uri_from_path(path)
        .map_err(|e| format!("Failed to create URI from path '{}': {e}", path.display()))
}

/// Extract project root path from the server's root_uri.
fn root_path(server: &lsp_client::LspServer) -> PathBuf {
    let root_uri = server.root_uri();
    if let Ok(url) = url::Url::parse(root_uri.as_str())
        && let Ok(path) = url.to_file_path()
    {
        return path;
    }
    PathBuf::from(".")
}

/// Validate that file_path, line, character are all present; return (path, line, character).
fn require_position(params: &LspParams) -> Result<(&str, u32, u32), String> {
    let file_path = params
        .file_path
        .as_deref()
        .ok_or("'filePath' is required for this operation")?;
    let line = params.line.ok_or("'line' is required for this operation")?;
    if line == 0 {
        return Err("'line' must be >= 1 (1-based)".to_string());
    }
    let character = params
        .character
        .ok_or("'character' is required for this operation")?;
    if character == 0 {
        return Err("'character' must be >= 1 (1-based)".to_string());
    }
    Ok((file_path, line, character))
}

/// Get a server for a file path, returning (server, filetype, absolute_path, uri).
async fn server_for_file(
    manager: &LspManager,
    permission: &ScopedPermissionManager,
    file_path: &str,
) -> Result<(Arc<lsp_client::LspServer>, String, PathBuf, lsp_types::Uri), String> {
    let abs_path = resolve_file_path(file_path).await?;
    crate::file_permission::check_file_read_permission(permission, &abs_path, abs_path.is_dir())
        .await
        .map_err(|e| format!("{e}"))?;
    let ext = get_extension(&abs_path)?;
    let filetype = manager
        .filetype_for_extension(&ext)
        .ok_or_else(|| format!("No LSP server configured for '{ext}' files"))?
        .to_string();
    let server = manager
        .get_or_start_server(&filetype)
        .await
        .map_err(|e| format!("Failed to start LSP server for '{filetype}': {e}"))?;
    let uri = path_to_uri(&abs_path)?;
    Ok((server, filetype, abs_path, uri))
}

/// Open a file with the LSP server, read content from disk.
async fn open_file(
    server: &lsp_client::LspServer,
    uri: &lsp_types::Uri,
    filetype: &str,
    abs_path: &Path,
) -> Result<(), String> {
    let content = tokio::fs::read_to_string(abs_path)
        .await
        .map_err(|e| format!("Failed to read file '{}': {e}", abs_path.display()))?;
    server
        .open_file(uri, filetype, &content)
        .await
        .map_err(|e| format!("Failed to open file in LSP server: {e}"))
}

/// Close a file with the LSP server, logging any errors.
async fn close_file(server: &lsp_client::LspServer, uri: &lsp_types::Uri) {
    if let Err(e) = server.close_file(uri).await {
        tracing::debug!("Failed to close file in LSP server: {e}");
    }
}

/// Convert an LSP URI to an absolute filesystem path, if possible.
fn uri_to_absolute_path(uri: &lsp_types::Uri) -> Option<PathBuf> {
    let url = url::Url::parse(uri.as_str()).ok()?;
    url.to_file_path().ok()
}

/// Check whether `path` is gitignored relative to `root`.
fn is_gitignored(path: &Path, root: &Path) -> bool {
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return false,
    };

    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);

    let mut current = root.to_path_buf();
    let gi = current.join(".gitignore");
    if gi.exists()
        && let Some(e) = builder.add(&gi)
    {
        tracing::debug!("Failed to parse .gitignore at {}: {e}", gi.display());
    }
    if let Some(parent) = rel.parent() {
        for component in parent.components() {
            current = current.join(component);
            let gi = current.join(".gitignore");
            if gi.exists()
                && let Some(e) = builder.add(&gi)
            {
                tracing::debug!("Failed to parse .gitignore at {}: {e}", gi.display());
            }
        }
    }

    let Ok(gitignore) = builder.build() else {
        return false;
    };
    gitignore.matched(path, false).is_ignore()
}

/// Return `true` if the file at the given URI is gitignored.
fn is_uri_gitignored(uri: &lsp_types::Uri, root: &Path) -> bool {
    uri_to_absolute_path(uri).is_some_and(|p| is_gitignored(&p, root))
}

/// Filter `Location`s, removing any whose URI points to a gitignored file.
async fn filter_locations(locations: Vec<Location>, root: &Path) -> Vec<Location> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        locations
            .into_iter()
            .filter(|loc| !is_uri_gitignored(&loc.uri, &root))
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Filter a `GotoDefinitionResponse`, removing gitignored locations.
async fn filter_definition_response(
    response: GotoDefinitionResponse,
    root: &Path,
) -> Option<GotoDefinitionResponse> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || match response {
        GotoDefinitionResponse::Scalar(loc) => {
            if is_uri_gitignored(&loc.uri, &root) {
                None
            } else {
                Some(GotoDefinitionResponse::Scalar(loc))
            }
        }
        GotoDefinitionResponse::Array(locs) => {
            let locs: Vec<_> = locs
                .into_iter()
                .filter(|loc| !is_uri_gitignored(&loc.uri, &root))
                .collect();
            if locs.is_empty() {
                None
            } else {
                Some(GotoDefinitionResponse::Array(locs))
            }
        }
        GotoDefinitionResponse::Link(links) => {
            let links: Vec<_> = links
                .into_iter()
                .filter(|link| !is_uri_gitignored(&link.target_uri, &root))
                .collect();
            if links.is_empty() {
                None
            } else {
                Some(GotoDefinitionResponse::Link(links))
            }
        }
    })
    .await
    .unwrap_or(None)
}

/// Filter incoming calls, removing gitignored callers.
async fn filter_incoming_calls(
    calls: Vec<CallHierarchyIncomingCall>,
    root: &Path,
) -> Vec<CallHierarchyIncomingCall> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        calls
            .into_iter()
            .filter(|call| !is_uri_gitignored(&call.from.uri, &root))
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Filter outgoing calls, removing gitignored targets.
async fn filter_outgoing_calls(
    calls: Vec<CallHierarchyOutgoingCall>,
    root: &Path,
) -> Vec<CallHierarchyOutgoingCall> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        calls
            .into_iter()
            .filter(|call| !is_uri_gitignored(&call.to.uri, &root))
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Filter workspace symbols, removing gitignored entries.
async fn filter_workspace_symbols(
    response: WorkspaceSymbolResponse,
    root: &Path,
) -> Option<WorkspaceSymbolResponse> {
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || match response {
        WorkspaceSymbolResponse::Flat(symbols) => {
            let symbols: Vec<_> = symbols
                .into_iter()
                .filter(|sym| !is_uri_gitignored(&sym.location.uri, &root))
                .collect();
            if symbols.is_empty() {
                None
            } else {
                Some(WorkspaceSymbolResponse::Flat(symbols))
            }
        }
        WorkspaceSymbolResponse::Nested(symbols) => {
            let symbols: Vec<_> = symbols
                .into_iter()
                .filter(|sym| {
                    let uri = match &sym.location {
                        lsp_types::OneOf::Left(loc) => &loc.uri,
                        lsp_types::OneOf::Right(uri_info) => &uri_info.uri,
                    };
                    !is_uri_gitignored(uri, &root)
                })
                .collect();
            if symbols.is_empty() {
                None
            } else {
                Some(WorkspaceSymbolResponse::Nested(symbols))
            }
        }
    })
    .await
    .unwrap_or(None)
}

/// Build a TextDocumentPositionParams from uri and 1-based line/character.
fn build_position_params(
    uri: &lsp_types::Uri,
    line: u32,
    character: u32,
) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position {
            line: line - 1,
            character: character - 1,
        },
    }
}

/// If the LSP server has active progress, append a warning to the result.
fn maybe_append_progress(result: String, server: &lsp_client::LspServer) -> String {
    let items = server.progress().active_items();
    if items.is_empty() {
        return result;
    }

    let mut warning =
        String::from("\n\nWARNING: LSP server has work in progress — results may be incomplete:");
    for item in &items {
        warning.push_str("\n  - ");
        warning.push_str(&item.title);
        if let Some(msg) = &item.message {
            warning.push_str(" (");
            warning.push_str(msg);
            warning.push(')');
        }
        if let Some(pct) = item.percentage {
            warning.push_str(&format!(" [{pct}%]"));
        }
    }

    format!("{result}{warning}")
}

async fn execute_lsp_operation(
    manager: &LspManager,
    permission: &ScopedPermissionManager,
    params: LspParams,
) -> Result<String, String> {
    if params.language.is_some() && !matches!(params.operation, LspOperation::WorkspaceSymbol) {
        return Err(
            "'language' parameter is only supported for workspaceSymbol operation".to_string(),
        );
    }

    let (result, server_opt) = match params.operation {
        LspOperation::GoToDefinition => {
            let (file_path, line, character) = require_position(&params)?;
            let (server, filetype, abs_path, uri) =
                server_for_file(manager, permission, file_path).await?;
            open_file(&server, &uri, &filetype, &abs_path).await?;

            let pos_params = build_position_params(&uri, line, character);
            let result = server
                .request::<lsp_types::request::GotoDefinition>(GotoDefinitionParams {
                    text_document_position_params: pos_params,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .await;

            close_file(&server, &uri).await;
            let result = result.map_err(|e| format!("goToDefinition request failed: {e}"))?;
            let root = root_path(&server);

            let result = match result {
                Some(response) => match filter_definition_response(response, &root).await {
                    Some(filtered) => Ok(formatters::format_definition(filtered, &root)),
                    None => Ok("No definitions found.".to_string()),
                },
                None => Ok("No definition found.".to_string()),
            };
            (result, Some(server))
        }

        LspOperation::FindReferences => {
            let (file_path, line, character) = require_position(&params)?;
            let (server, filetype, abs_path, uri) =
                server_for_file(manager, permission, file_path).await?;
            open_file(&server, &uri, &filetype, &abs_path).await?;

            let pos_params = build_position_params(&uri, line, character);
            let result = server
                .request::<lsp_types::request::References>(ReferenceParams {
                    text_document_position: pos_params,
                    context: ReferenceContext {
                        include_declaration: true,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .await;

            close_file(&server, &uri).await;
            let result = result.map_err(|e| format!("findReferences request failed: {e}"))?;
            let root = root_path(&server);

            let result = match result {
                Some(locations) => {
                    let locations = filter_locations(locations, &root).await;
                    if locations.is_empty() {
                        Ok("No references found.".to_string())
                    } else {
                        Ok(formatters::format_references(locations, &root))
                    }
                }
                None => Ok("No references found.".to_string()),
            };
            (result, Some(server))
        }

        LspOperation::Hover => {
            let (file_path, line, character) = require_position(&params)?;
            let (server, filetype, abs_path, uri) =
                server_for_file(manager, permission, file_path).await?;
            open_file(&server, &uri, &filetype, &abs_path).await?;

            let pos_params = build_position_params(&uri, line, character);
            let result = server
                .request::<lsp_types::request::HoverRequest>(HoverParams {
                    text_document_position_params: pos_params,
                    work_done_progress_params: Default::default(),
                })
                .await;

            close_file(&server, &uri).await;
            let result = result.map_err(|e| format!("hover request failed: {e}"))?;

            let result = match result {
                Some(hover) => Ok(formatters::format_hover(hover)),
                None => Ok("No hover information available.".to_string()),
            };
            (result, Some(server))
        }

        LspOperation::GoToImplementation => {
            let (file_path, line, character) = require_position(&params)?;
            let (server, filetype, abs_path, uri) =
                server_for_file(manager, permission, file_path).await?;
            open_file(&server, &uri, &filetype, &abs_path).await?;

            let pos_params = build_position_params(&uri, line, character);
            let result = server
                .request::<lsp_types::request::GotoImplementation>(
                    lsp_types::request::GotoImplementationParams {
                        text_document_position_params: pos_params,
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    },
                )
                .await;

            close_file(&server, &uri).await;
            let result = result.map_err(|e| format!("goToImplementation request failed: {e}"))?;
            let root = root_path(&server);

            let result = match result {
                Some(response) => match filter_definition_response(response, &root).await {
                    Some(filtered) => Ok(formatters::format_definition(filtered, &root)),
                    None => Ok("No implementations found.".to_string()),
                },
                None => Ok("No implementations found.".to_string()),
            };
            (result, Some(server))
        }

        LspOperation::PrepareCallHierarchy => {
            let (file_path, line, character) = require_position(&params)?;
            let (server, filetype, abs_path, uri) =
                server_for_file(manager, permission, file_path).await?;
            open_file(&server, &uri, &filetype, &abs_path).await?;

            let pos_params = build_position_params(&uri, line, character);
            let result = server
                .request::<lsp_types::request::CallHierarchyPrepare>(
                    lsp_types::CallHierarchyPrepareParams {
                        text_document_position_params: pos_params,
                        work_done_progress_params: Default::default(),
                    },
                )
                .await;

            close_file(&server, &uri).await;
            let result = result.map_err(|e| format!("prepareCallHierarchy request failed: {e}"))?;
            let root = root_path(&server);

            let result = match result {
                Some(items) if !items.is_empty() => {
                    let mut out = String::from("Call hierarchy items:\n");
                    for item in &items {
                        out.push_str(&format!(
                            "  {}\n",
                            formatters::format_call_hierarchy_item(item, &root)
                        ));
                    }
                    Ok(out)
                }
                _ => Ok("No call hierarchy item found at this position.".to_string()),
            };
            (result, Some(server))
        }

        LspOperation::IncomingCalls => {
            let (file_path, line, character) = require_position(&params)?;
            let (server, filetype, abs_path, uri) =
                server_for_file(manager, permission, file_path).await?;
            open_file(&server, &uri, &filetype, &abs_path).await?;

            let pos_params = build_position_params(&uri, line, character);

            // Step 1: prepareCallHierarchy
            let prepare_result = server
                .request::<lsp_types::request::CallHierarchyPrepare>(
                    lsp_types::CallHierarchyPrepareParams {
                        text_document_position_params: pos_params,
                        work_done_progress_params: Default::default(),
                    },
                )
                .await;

            let prepare_result =
                prepare_result.map_err(|e| format!("prepareCallHierarchy request failed: {e}"))?;

            let items = match prepare_result {
                Some(items) if !items.is_empty() => items,
                _ => {
                    close_file(&server, &uri).await;
                    return Ok(maybe_append_progress(
                        "No call hierarchy item found at this position.".to_string(),
                        &server,
                    ));
                }
            };

            // Step 2: incomingCalls using the first item
            let result = server
                .request::<lsp_types::request::CallHierarchyIncomingCalls>(
                    CallHierarchyIncomingCallsParams {
                        item: items.into_iter().next().expect("checked non-empty above"),
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    },
                )
                .await;

            close_file(&server, &uri).await;
            let result = result.map_err(|e| format!("incomingCalls request failed: {e}"))?;
            let root = root_path(&server);

            let result = match result {
                Some(calls) => {
                    let calls = filter_incoming_calls(calls, &root).await;
                    if calls.is_empty() {
                        Ok("No incoming calls found.".to_string())
                    } else {
                        Ok(formatters::format_incoming_calls(calls, &root))
                    }
                }
                None => Ok("No incoming calls found.".to_string()),
            };
            (result, Some(server))
        }

        LspOperation::OutgoingCalls => {
            let (file_path, line, character) = require_position(&params)?;
            let (server, filetype, abs_path, uri) =
                server_for_file(manager, permission, file_path).await?;
            open_file(&server, &uri, &filetype, &abs_path).await?;

            let pos_params = build_position_params(&uri, line, character);

            // Step 1: prepareCallHierarchy
            let prepare_result = server
                .request::<lsp_types::request::CallHierarchyPrepare>(
                    lsp_types::CallHierarchyPrepareParams {
                        text_document_position_params: pos_params,
                        work_done_progress_params: Default::default(),
                    },
                )
                .await;

            let prepare_result =
                prepare_result.map_err(|e| format!("prepareCallHierarchy request failed: {e}"))?;

            let items = match prepare_result {
                Some(items) if !items.is_empty() => items,
                _ => {
                    close_file(&server, &uri).await;
                    return Ok(maybe_append_progress(
                        "No call hierarchy item found at this position.".to_string(),
                        &server,
                    ));
                }
            };

            // Step 2: outgoingCalls using the first item
            let result = server
                .request::<lsp_types::request::CallHierarchyOutgoingCalls>(
                    CallHierarchyOutgoingCallsParams {
                        item: items.into_iter().next().expect("checked non-empty above"),
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    },
                )
                .await;

            close_file(&server, &uri).await;
            let result = result.map_err(|e| format!("outgoingCalls request failed: {e}"))?;
            let root = root_path(&server);

            let result = match result {
                Some(calls) => {
                    let calls = filter_outgoing_calls(calls, &root).await;
                    if calls.is_empty() {
                        Ok("No outgoing calls found.".to_string())
                    } else {
                        Ok(formatters::format_outgoing_calls(calls, &root))
                    }
                }
                None => Ok("No outgoing calls found.".to_string()),
            };
            (result, Some(server))
        }

        LspOperation::DocumentSymbol => {
            let file_path = params
                .file_path
                .as_deref()
                .ok_or("'filePath' is required for documentSymbol")?;
            let (server, filetype, abs_path, uri) =
                server_for_file(manager, permission, file_path).await?;
            open_file(&server, &uri, &filetype, &abs_path).await?;

            let result = server
                .request::<lsp_types::request::DocumentSymbolRequest>(DocumentSymbolParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .await;

            close_file(&server, &uri).await;
            let result = result.map_err(|e| format!("documentSymbol request failed: {e}"))?;

            let result = match result {
                Some(response) => Ok(formatters::format_document_symbols(response, file_path)),
                None => Ok(format!("No symbols found in {file_path}.")),
            };
            (result, Some(server))
        }

        LspOperation::WorkspaceSymbol => {
            let query = params.query.as_deref().unwrap_or("").to_string();

            // Priority: language > filePath > error
            let server = if let Some(language) =
                params.language.as_deref().filter(|s| !s.is_empty())
            {
                // Use language directly as filetype
                manager.get_or_start_server(language).await.map_err(|e| {
                    format!("Failed to start LSP server for language '{language}': {e}")
                })?
            } else if let Some(file_path) = params.file_path.as_deref() {
                // Derive filetype from file extension
                let abs_path = resolve_file_path(file_path).await?;
                crate::file_permission::check_file_read_permission(
                    permission,
                    &abs_path,
                    abs_path.is_dir(),
                )
                .await
                .map_err(|e| format!("{e}"))?;
                let ext = get_extension(&abs_path)?;
                let filetype = manager
                    .filetype_for_extension(&ext)
                    .ok_or_else(|| format!("No LSP server configured for '{ext}' files"))?;
                manager
                    .get_or_start_server(filetype)
                    .await
                    .map_err(|e| format!("Failed to start LSP server for '{filetype}': {e}"))?
            } else {
                return Err(
                    "workspaceSymbol requires either 'language' or 'filePath' to determine which LSP server to query"
                        .to_string(),
                );
            };

            let result = server
                .request::<lsp_types::request::WorkspaceSymbolRequest>(WorkspaceSymbolParams {
                    query,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                })
                .await
                .map_err(|e| format!("workspaceSymbol request failed: {e}"))?;

            let root = root_path(&server);

            let result = match result {
                Some(response) => match filter_workspace_symbols(response, &root).await {
                    Some(filtered) => Ok(formatters::format_workspace_symbols(filtered, &root)),
                    None => Ok("No symbols found.".to_string()),
                },
                None => Ok("No symbols found.".to_string()),
            };
            (result, Some(server))
        }
    };

    match (result, server_opt) {
        (Ok(text), Some(server)) => Ok(maybe_append_progress(text, &server)),
        (other, _) => other,
    }
}
