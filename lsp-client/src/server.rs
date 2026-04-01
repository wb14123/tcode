use std::path::Path;

use anyhow::{Context, Result};
use lsp_types::{
    CallHierarchyClientCapabilities, ClientCapabilities, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, GeneralClientCapabilities, GotoCapability, InitializeParams,
    InitializeResult, InitializedParams, TextDocumentClientCapabilities, TextDocumentIdentifier,
    TextDocumentItem, Uri, WindowClientCapabilities, WorkspaceClientCapabilities,
    notification::Notification, request::Request,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config::LspServerConfig;
use crate::transport::LspTransport;

/// A running LSP server instance.
pub struct LspServer {
    transport: LspTransport,
    server_capabilities: lsp_types::ServerCapabilities,
    config: LspServerConfig,
    root_uri: Uri,
}

/// Convert a filesystem path to a `file://` URI.
pub fn uri_from_path(path: &Path) -> Result<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let url = url::Url::from_file_path(&abs)
        .map_err(|()| anyhow::anyhow!("Invalid path for URI: {}", abs.display()))?;
    url.as_str()
        .parse::<Uri>()
        .map_err(|e| anyhow::anyhow!("Failed to parse URI: {e}"))
}

impl LspServer {
    /// Start a new LSP server process and complete the initialization handshake.
    pub async fn start(config: &LspServerConfig, root_dir: &Path) -> Result<Self> {
        if config.cmd.is_empty() {
            anyhow::bail!("LSP server '{}' has no command configured", config.name);
        }

        let child = tokio::process::Command::new(&config.cmd[0])
            .args(&config.cmd[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to spawn LSP server: {}", config.cmd.join(" ")))?;

        let transport = LspTransport::new(child).await?;

        let root_uri = uri_from_path(root_dir)?;

        // Build initialization options
        let init_options = build_init_options(config);

        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri.clone()),
            capabilities: build_client_capabilities(),
            initialization_options: init_options,
            ..Default::default()
        };

        let params_value = serde_json::to_value(&params)?;
        let result_value = transport.send_request("initialize", params_value).await?;
        let init_result: InitializeResult = serde_json::from_value(result_value)
            .context("Failed to deserialize initialize response")?;

        // Send initialized notification
        let initialized_params = serde_json::to_value(InitializedParams {})?;
        transport
            .send_notification("initialized", initialized_params)
            .await?;

        // If the server has settings, send workspace/didChangeConfiguration
        if let Some(settings) = &config.settings {
            let change_params = serde_json::json!({ "settings": settings });
            transport
                .send_notification("workspace/didChangeConfiguration", change_params)
                .await?;
        }

        Ok(Self {
            transport,
            server_capabilities: init_result.capabilities,
            config: config.clone(),
            root_uri,
        })
    }

    /// Send a typed LSP request and deserialize the response.
    pub async fn request<R>(&self, params: R::Params) -> Result<R::Result>
    where
        R: Request,
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        let params_value = serde_json::to_value(params)?;
        let result_value = self.transport.send_request(R::METHOD, params_value).await?;
        let result: R::Result = serde_json::from_value(result_value)
            .with_context(|| format!("Failed to deserialize response for {}", R::METHOD))?;
        Ok(result)
    }

    /// Send a typed LSP notification.
    pub async fn notify<N>(&self, params: N::Params) -> Result<()>
    where
        N: Notification,
        N::Params: Serialize,
    {
        let params_value = serde_json::to_value(params)?;
        self.transport
            .send_notification(N::METHOD, params_value)
            .await
    }

    /// Notify the server that a file was opened.
    pub async fn open_file(&self, uri: &Uri, language_id: &str, content: &str) -> Result<()> {
        self.notify::<lsp_types::notification::DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: language_id.to_string(),
                version: 1,
                text: content.to_string(),
            },
        })
        .await
    }

    /// Notify the server that a file was closed.
    pub async fn close_file(&self, uri: &Uri) -> Result<()> {
        self.notify::<lsp_types::notification::DidCloseTextDocument>(DidCloseTextDocumentParams {
            text_document: TextDocumentIdentifier { uri: uri.clone() },
        })
        .await
    }

    /// Shut down this LSP server.
    pub async fn shutdown(self) -> Result<()> {
        self.transport.shutdown().await
    }

    /// Get the server's capabilities.
    pub fn capabilities(&self) -> &lsp_types::ServerCapabilities {
        &self.server_capabilities
    }

    /// Get the root URI for this server.
    pub fn root_uri(&self) -> &Uri {
        &self.root_uri
    }

    /// Get the server config.
    pub fn config(&self) -> &LspServerConfig {
        &self.config
    }
}

fn build_client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        general: Some(GeneralClientCapabilities {
            ..Default::default()
        }),
        window: Some(WindowClientCapabilities {
            work_done_progress: Some(true),
            ..Default::default()
        }),
        text_document: Some(TextDocumentClientCapabilities {
            definition: Some(GotoCapability {
                dynamic_registration: Some(false),
                link_support: Some(false),
            }),
            references: Some(lsp_types::DynamicRegistrationClientCapabilities {
                dynamic_registration: Some(false),
            }),
            hover: Some(lsp_types::HoverClientCapabilities {
                dynamic_registration: Some(false),
                content_format: Some(vec![lsp_types::MarkupKind::PlainText]),
            }),
            document_symbol: Some(lsp_types::DocumentSymbolClientCapabilities {
                dynamic_registration: Some(false),
                hierarchical_document_symbol_support: Some(true),
                ..Default::default()
            }),
            implementation: Some(GotoCapability {
                dynamic_registration: Some(false),
                link_support: Some(false),
            }),
            call_hierarchy: Some(CallHierarchyClientCapabilities {
                dynamic_registration: Some(false),
            }),
            ..Default::default()
        }),
        workspace: Some(WorkspaceClientCapabilities {
            symbol: Some(lsp_types::WorkspaceSymbolClientCapabilities {
                dynamic_registration: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn build_init_options(config: &LspServerConfig) -> Option<Value> {
    let mut options = config.init_options.clone();

    // For rust-analyzer, merge checkOnSave: false
    if config.name == "rust_analyzer" || config.name == "rust-analyzer" {
        let opts = options.get_or_insert_with(|| serde_json::json!({}));
        if let Some(obj) = opts.as_object_mut() {
            obj.entry("checkOnSave").or_insert(serde_json::json!(false));
        }
    }

    options
}
