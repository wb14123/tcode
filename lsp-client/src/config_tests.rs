use crate::config::parse_config_json;

const FULL_CONFIG_JSON: &str = r#"{
  "servers": {
    "rust_analyzer": {
      "cmd": ["rust-analyzer"],
      "filetypes": ["rust"],
      "root_markers": ["Cargo.toml"],
      "settings": null,
      "init_options": null
    },
    "lua_ls": {
      "cmd": ["lua-language-server"],
      "filetypes": ["lua"],
      "root_markers": [".luarc.json"],
      "settings": {"Lua": {"diagnostics": {"globals": ["vim"]}}},
      "init_options": null
    }
  },
  "extensions": {
    ".rs": "rust",
    ".lua": "lua"
  }
}"#;

#[test]
fn parse_valid_config_with_multiple_servers() -> anyhow::Result<()> {
    let config = parse_config_json(FULL_CONFIG_JSON)?;

    assert_eq!(config.servers.len(), 2);
    assert!(config.has_servers());

    // Find each server by name (HashMap iteration order is non-deterministic)
    let rust = config.servers.iter().find(|s| s.name == "rust_analyzer");
    let lua = config.servers.iter().find(|s| s.name == "lua_ls");

    let rust = rust.expect("rust_analyzer should be present");
    assert_eq!(rust.cmd, vec!["rust-analyzer"]);
    assert_eq!(rust.filetypes, vec!["rust"]);
    assert_eq!(rust.root_markers, vec!["Cargo.toml"]);
    assert!(rust.settings.is_none());
    assert!(rust.init_options.is_none());

    let lua = lua.expect("lua_ls should be present");
    assert_eq!(lua.cmd, vec!["lua-language-server"]);
    assert_eq!(lua.filetypes, vec!["lua"]);
    assert_eq!(lua.root_markers, vec![".luarc.json"]);
    assert!(lua.settings.is_some());
    assert!(lua.init_options.is_none());

    // Verify nested settings structure
    let settings = lua.settings.as_ref().expect("lua_ls settings should exist");
    assert_eq!(
        settings["Lua"]["diagnostics"]["globals"][0],
        serde_json::Value::String("vim".to_string())
    );

    Ok(())
}

#[test]
fn parse_empty_servers() -> anyhow::Result<()> {
    let json = r#"{"servers": {}, "extensions": {}}"#;
    let config = parse_config_json(json)?;

    assert!(config.servers.is_empty());
    assert!(!config.has_servers());
    assert!(config.extension_to_filetype.is_empty());

    Ok(())
}

#[test]
fn parse_missing_optional_fields() -> anyhow::Result<()> {
    // No settings or init_options fields at all (not even null)
    let json = r#"{
      "servers": {
        "pyright": {
          "cmd": ["pyright-langserver", "--stdio"],
          "filetypes": ["python"],
          "root_markers": ["pyproject.toml"]
        }
      },
      "extensions": {".py": "python"}
    }"#;
    let config = parse_config_json(json)?;

    assert_eq!(config.servers.len(), 1);
    let srv = &config.servers[0];
    assert_eq!(srv.name, "pyright");
    assert_eq!(srv.cmd, vec!["pyright-langserver", "--stdio"]);
    assert!(srv.settings.is_none());
    assert!(srv.init_options.is_none());

    Ok(())
}

#[test]
fn parse_missing_filetypes_and_root_markers_defaults_to_empty() -> anyhow::Result<()> {
    // filetypes and root_markers are serde(default) so omitting them should yield empty vecs
    let json = r#"{
      "servers": {
        "custom_server": {
          "cmd": ["my-server"]
        }
      },
      "extensions": {}
    }"#;
    let config = parse_config_json(json)?;

    assert_eq!(config.servers.len(), 1);
    let srv = &config.servers[0];
    assert_eq!(srv.name, "custom_server");
    assert!(srv.filetypes.is_empty());
    assert!(srv.root_markers.is_empty());

    Ok(())
}

#[test]
fn has_servers_returns_correct_values() -> anyhow::Result<()> {
    let empty = parse_config_json(r#"{"servers": {}, "extensions": {}}"#)?;
    assert!(!empty.has_servers());

    let non_empty = parse_config_json(r#"{"servers": {"x": {"cmd": ["x"]}}, "extensions": {}}"#)?;
    assert!(non_empty.has_servers());

    Ok(())
}

#[test]
fn extension_to_filetype_mapping() -> anyhow::Result<()> {
    let config = parse_config_json(FULL_CONFIG_JSON)?;

    assert_eq!(config.extension_to_filetype.len(), 2);
    assert_eq!(
        config.extension_to_filetype.get(".rs").map(String::as_str),
        Some("rust")
    );
    assert_eq!(
        config.extension_to_filetype.get(".lua").map(String::as_str),
        Some("lua")
    );
    assert!(!config.extension_to_filetype.contains_key(".py"));

    Ok(())
}

#[test]
fn malformed_json_returns_empty_config() -> anyhow::Result<()> {
    let config = parse_config_json("not valid json at all")?;

    assert!(!config.has_servers());
    assert!(config.servers.is_empty());
    assert!(config.extension_to_filetype.is_empty());

    Ok(())
}

#[test]
fn incomplete_json_structure_returns_empty_config() -> anyhow::Result<()> {
    // Valid JSON but wrong structure (missing required "servers" field)
    let config = parse_config_json(r#"{"extensions": {".rs": "rust"}}"#)?;

    assert!(!config.has_servers());
    assert!(config.extension_to_filetype.is_empty());

    Ok(())
}

#[test]
fn server_with_empty_cmd_array() -> anyhow::Result<()> {
    let json = r#"{
      "servers": {
        "broken": {
          "cmd": [],
          "filetypes": ["rust"],
          "root_markers": []
        }
      },
      "extensions": {}
    }"#;
    let config = parse_config_json(json)?;

    assert_eq!(config.servers.len(), 1);
    assert!(config.servers[0].cmd.is_empty());

    Ok(())
}

#[test]
fn server_with_empty_filetypes_array() -> anyhow::Result<()> {
    let json = r#"{
      "servers": {
        "generic": {
          "cmd": ["generic-ls"],
          "filetypes": [],
          "root_markers": [".git"]
        }
      },
      "extensions": {}
    }"#;
    let config = parse_config_json(json)?;

    assert_eq!(config.servers.len(), 1);
    let srv = &config.servers[0];
    assert_eq!(srv.name, "generic");
    assert!(srv.filetypes.is_empty());
    assert_eq!(srv.root_markers, vec![".git"]);

    Ok(())
}

#[test]
fn server_with_init_options() -> anyhow::Result<()> {
    let json = r#"{
      "servers": {
        "gopls": {
          "cmd": ["gopls"],
          "filetypes": ["go"],
          "root_markers": ["go.mod"],
          "settings": null,
          "init_options": {"usePlaceholders": true, "completeUnimported": true}
        }
      },
      "extensions": {".go": "go"}
    }"#;
    let config = parse_config_json(json)?;

    let srv = &config.servers[0];
    let init_opts = srv
        .init_options
        .as_ref()
        .expect("init_options should be present");
    assert_eq!(init_opts["usePlaceholders"], serde_json::Value::Bool(true));
    assert_eq!(
        init_opts["completeUnimported"],
        serde_json::Value::Bool(true)
    );

    Ok(())
}

#[test]
fn multiple_extensions_same_filetype() -> anyhow::Result<()> {
    let json = r#"{
      "servers": {
        "tsserver": {
          "cmd": ["typescript-language-server", "--stdio"],
          "filetypes": ["typescript", "typescriptreact"],
          "root_markers": ["tsconfig.json"]
        }
      },
      "extensions": {".ts": "typescript", ".tsx": "typescriptreact", ".mts": "typescript"}
    }"#;
    let config = parse_config_json(json)?;

    assert_eq!(config.extension_to_filetype.len(), 3);
    assert_eq!(config.extension_to_filetype[".ts"], "typescript");
    assert_eq!(config.extension_to_filetype[".tsx"], "typescriptreact");
    assert_eq!(config.extension_to_filetype[".mts"], "typescript");

    Ok(())
}
