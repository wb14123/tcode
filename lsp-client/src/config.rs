use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use serde::Deserialize;

/// Configuration for a single LSP server.
#[derive(Debug, Clone, Deserialize)]
pub struct LspServerConfig {
    pub name: String,
    pub cmd: Vec<String>,
    pub filetypes: Vec<String>,
    pub root_markers: Vec<String>,
    pub settings: Option<serde_json::Value>,
    pub init_options: Option<serde_json::Value>,
}

/// Aggregated LSP configuration.
#[derive(Debug, Clone)]
pub struct LspConfig {
    pub servers: Vec<LspServerConfig>,
    pub extension_to_filetype: HashMap<String, String>,
}

impl LspConfig {
    pub fn has_servers(&self) -> bool {
        !self.servers.is_empty()
    }
}

/// Raw server config as parsed from nvim JSON output.
#[derive(Debug, Deserialize)]
struct RawServerConfig {
    cmd: Vec<String>,
    #[serde(default)]
    filetypes: Vec<String>,
    #[serde(default)]
    root_markers: Vec<String>,
    settings: Option<serde_json::Value>,
    init_options: Option<serde_json::Value>,
}

/// Top-level JSON output from nvim.
#[derive(Debug, Deserialize)]
struct NvimOutput {
    servers: HashMap<String, RawServerConfig>,
    extensions: HashMap<String, String>,
}

/// Extract LSP configuration from neovim's built-in LSP configs.
///
/// Runs a headless nvim process to discover installed LSP servers,
/// their filetypes, root markers, settings, and file extension mappings.
pub async fn extract_config_from_nvim() -> Result<LspConfig> {
    let lua_script = r#"
-- Force-load nvim-lspconfig if managed by lazy.nvim (it's often lazy-loaded,
-- so vim.lsp.config won't have server definitions during headless startup).
-- Other plugin managers:
--   packer.nvim: pcall(function() require("packer").loader("nvim-lspconfig") end)
--   mini.deps: plugins are typically eager-loaded, no action needed
pcall(function() require("lazy").load({plugins = {"nvim-lspconfig"}}) end)

-- Trigger the plugin's config function which calls vim.lsp.enable() for each
-- server. In headless mode the "LazyFile" event never fires, so we manually
-- fire FileType + a short wait to let vim.schedule_wrap callbacks execute.
vim.cmd("do FileType")
vim.wait(200, function() return false end)

-- Recursively strip non-serializable values (functions, userdata, threads)
-- from a table so vim.json.encode won't error. Many LSP configs contain
-- callbacks (on_attach, before_init, handlers, etc.) that we can't serialize.
local function sanitize(tbl)
  if type(tbl) ~= "table" then return tbl end
  local result = {}
  for k, v in pairs(tbl) do
    local t = type(v)
    if t == "function" or t == "userdata" or t == "thread" then
      -- skip
    elseif t == "table" then
      result[k] = sanitize(v)
    else
      result[k] = v
    end
  end
  return result
end

-- Flatten root_markers: some LSP configs use nested arrays for groups of
-- alternatives (e.g. lua_ls: {{".luarc.json",".luarc.jsonc"}, {".git"}}).
-- We flatten these into a simple list of strings.
local function flatten_root_markers(markers)
  if type(markers) ~= "table" then return {} end
  local result = {}
  for _, item in ipairs(markers) do
    if type(item) == "string" then
      result[#result + 1] = item
    elseif type(item) == "table" then
      for _, sub in ipairs(item) do
        if type(sub) == "string" then
          result[#result + 1] = sub
        end
      end
    end
  end
  return result
end

-- Get the list of user-enabled server names from nvim's internal registry.
-- vim.lsp._enabled_configs is populated by vim.lsp.enable() calls.
local enabled = vim.lsp._enabled_configs or {}

local servers = {}
for name, _ in pairs(enabled) do
  -- Resolve the full merged config (lsp/*.lua base + user overrides)
  local cfg = vim.lsp.config[name]
  if cfg and type(cfg.cmd) == "table" then
    -- Only include servers whose command is actually executable
    if cfg.cmd[1] and vim.fn.executable(cfg.cmd[1]) == 1 then
      servers[name] = {
        cmd = cfg.cmd,
        filetypes = cfg.filetypes or {},
        root_markers = flatten_root_markers(cfg.root_markers),
        settings = sanitize(cfg.settings),
        init_options = sanitize(cfg.init_options),
      }
    end
  end
end

-- Build extension-to-filetype map for all filetypes covered by enabled servers
local all_filetypes = {}
for _, srv in pairs(servers) do
  for _, ft in ipairs(srv.filetypes) do
    all_filetypes[ft] = true
  end
end
local test_extensions = {
  ".rs", ".py", ".pyi", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs",
  ".go", ".c", ".cpp", ".cc", ".h", ".hpp", ".java", ".kt", ".rb",
  ".lua", ".zig", ".sh", ".bash", ".toml", ".yaml", ".yml", ".json",
  ".html", ".css", ".svelte", ".vue", ".dart", ".swift", ".cs",
  ".ex", ".exs", ".erl", ".hs", ".ml", ".tf", ".proto", ".sql",
  ".php", ".scala", ".clj", ".elm", ".nim", ".r", ".mts", ".cts",
}
local exts = {}
for _, ext in ipairs(test_extensions) do
  local ft = vim.filetype.match({filename = "test" .. ext})
  if ft and all_filetypes[ft] then
    exts[ext] = ft
  end
end

io.stdout:write(vim.json.encode({ servers = servers, extensions = exts }))
"#;

    // Write the Lua script to a temp file under ~/.tcode/tmp/ because nvim's
    // `-c` flag only accepts single-line Ex commands — heredoc syntax doesn't work.
    let tmp_dir = tcode_tmp_dir()?;
    tokio::fs::create_dir_all(&tmp_dir).await?;
    let script_path = tmp_dir.join("lsp-config-extract.lua");
    tokio::fs::write(&script_path, lua_script).await?;

    let luafile_cmd = format!("luafile {}", script_path.display());
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("nvim")
            .args(["--headless", "-c", &luafile_cmd, "-c", "qa!"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;

    // Clean up the temp script (best-effort)
    if let Err(e) = tokio::fs::remove_file(&script_path).await {
        tracing::debug!("Failed to remove temp LSP config script: {e}");
    }

    let output = match result {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            tracing::warn!("Failed to run nvim for LSP config extraction: {e}");
            return Ok(empty_config());
        }
        Err(_) => {
            tracing::warn!("nvim LSP config extraction timed out after 10s");
            return Ok(empty_config());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(
            "nvim exited with status {} during LSP config extraction. stderr: {}",
            output.status,
            stderr.trim()
        );
        return Ok(empty_config());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        tracing::debug!("nvim LSP config extraction stderr: {}", stderr.trim());
    }
    // The JSON output is on stdout (via io.stdout:write, not print which goes to stderr in headless mode)
    let json_line = stdout.lines().rfind(|l| !l.trim().is_empty());
    let Some(json_str) = json_line else {
        tracing::warn!(
            "nvim produced no output for LSP config extraction. stdout={}, stderr={}",
            stdout.trim(),
            stderr.trim()
        );
        return Ok(empty_config());
    };

    parse_config_json(json_str)
}

/// Parse the JSON output from nvim into an `LspConfig`.
///
/// On malformed JSON, logs a warning and returns an empty config.
pub(crate) fn parse_config_json(json_str: &str) -> Result<LspConfig> {
    let nvim_output: NvimOutput = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse nvim LSP config JSON: {e}");
            return Ok(empty_config());
        }
    };

    let servers = nvim_output
        .servers
        .into_iter()
        .map(|(name, raw)| LspServerConfig {
            name,
            cmd: raw.cmd,
            filetypes: raw.filetypes,
            root_markers: raw.root_markers,
            settings: raw.settings,
            init_options: raw.init_options,
        })
        .collect();

    Ok(LspConfig {
        servers,
        extension_to_filetype: nvim_output.extensions,
    })
}

fn empty_config() -> LspConfig {
    LspConfig {
        servers: Vec::new(),
        extension_to_filetype: HashMap::new(),
    }
}

fn tcode_tmp_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    Ok(PathBuf::from(home).join(".tcode").join("tmp"))
}
