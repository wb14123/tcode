use super::config::*;

#[test]
fn test_default_layout_validates() -> anyhow::Result<()> {
    LayoutNode::default_layout().validate()
}

#[test]
fn test_parse_minimal_config() -> anyhow::Result<()> {
    let toml_str = "";
    let config: TcodeConfig = toml::from_str(toml_str)?;
    assert!(config.provider.is_none());
    assert!(config.layout.is_none());
    Ok(())
}

#[test]
fn test_parse_config_with_layout() -> anyhow::Result<()> {
    let toml_str = r#"
provider = "claude"
model = "claude-opus-4-6"

[layout]
split = "horizontal"

  [layout.a]
  command = "display"
  size = 70

  [layout.b]
  split = "vertical"
  size = 30

    [layout.b.a]
    command = "edit"
    size = 50
    focus = true

    [layout.b.b]
    command = "tree"
    size = 50
"#;
    let config: TcodeConfig = toml::from_str(toml_str)?;
    assert_eq!(config.provider.as_deref(), Some("claude"));
    let layout = config.layout.unwrap();
    layout.validate()?;
    Ok(())
}

#[test]
fn test_unknown_config_key_rejected() {
    let toml_str = r#"providre = "claude""#; // typo
    let result: Result<TcodeConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn test_unknown_layout_key_rejected() {
    let toml_str = r#"
[layout]
direction = "horizontal"
"#;
    let result: Result<TcodeConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn test_split_missing_children() {
    let toml_str = r#"
[layout]
split = "horizontal"
"#;
    let result: Result<TcodeConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn test_both_split_and_command() {
    let toml_str = r#"
[layout]
split = "horizontal"
command = "display"
"#;
    let result: Result<TcodeConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn test_neither_split_nor_command() {
    let toml_str = r#"
[layout]
size = 50
"#;
    let result: Result<TcodeConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn test_leaf_with_children_rejected() {
    let toml_str = r#"
[layout]
command = "display"

[layout.a]
command = "edit"
"#;
    let result: Result<TcodeConfig, _> = toml::from_str(toml_str);
    assert!(result.is_err());
}

#[test]
fn test_validation_no_display() {
    let layout = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Edit,
            size: Some(50),
            focus: None,
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Tree,
            size: Some(50),
            focus: None,
        }),
    };
    assert!(layout.validate().is_err());
}

#[test]
fn test_validation_two_displays() {
    let layout = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(50),
            focus: None,
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(50),
            focus: None,
        }),
    };
    assert!(layout.validate().is_err());
}

#[test]
fn test_validation_two_focus() {
    let layout = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(50),
            focus: Some(true),
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Edit,
            size: Some(50),
            focus: Some(true),
        }),
    };
    assert!(layout.validate().is_err());
}

#[test]
fn test_validation_size_zero() {
    let full = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(0),
            focus: None,
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Edit,
            size: Some(50),
            focus: None,
        }),
    };
    assert!(full.validate().is_err());
}

#[test]
fn test_validation_size_100() {
    let full = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(100),
            focus: None,
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Edit,
            size: Some(50),
            focus: None,
        }),
    };
    assert!(full.validate().is_err());
}

#[test]
fn test_validation_no_edit() {
    let layout = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(50),
            focus: None,
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Tree,
            size: Some(50),
            focus: None,
        }),
    };
    assert!(layout.validate().is_err());
}

#[test]
fn test_search_engine_str_defaults() {
    let config = TcodeConfig::default();
    assert_eq!(config.search_engine_str(), "kagi");
}

#[test]
fn test_nonexistent_profile_errors() {
    let result = super::config::load_config(Some("nonexistent-test-profile-xyz"));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("config not found at") && msg.contains("nonexistent-test-profile-xyz"),
        "unexpected error: {msg}"
    );
}

#[test]
fn test_focus_false_not_counted() -> anyhow::Result<()> {
    // `focus = false` is accepted but has no effect — it doesn't count toward focus_count
    let layout = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(50),
            focus: Some(false),
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Edit,
            size: Some(50),
            focus: Some(false),
        }),
    };
    layout.validate()?;
    Ok(())
}

#[test]
fn test_sibling_sizes_must_add_to_100() {
    let layout = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(70),
            focus: None,
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Edit,
            size: Some(50),
            focus: None,
        }),
    };
    let err = layout.validate().unwrap_err().to_string();
    assert!(
        err.contains("sibling sizes must add up to 100"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_sibling_sizes_valid_when_both_present() -> anyhow::Result<()> {
    let layout = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: Some(70),
            focus: None,
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Edit,
            size: Some(30),
            focus: None,
        }),
    };
    layout.validate()?;
    Ok(())
}

#[test]
fn test_sibling_sizes_ok_when_only_one_specified() -> anyhow::Result<()> {
    let layout = LayoutNode::Split {
        split: SplitDirection::Horizontal,
        size: None,
        a: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Display,
            size: None,
            focus: None,
        }),
        b: Box::new(LayoutNode::Leaf {
            command: PanelCommand::Edit,
            size: Some(30),
            focus: None,
        }),
    };
    layout.validate()?;
    Ok(())
}
