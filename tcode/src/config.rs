use anyhow::{Result, bail};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TcodeConfig {
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub subagent_max_iterations: Option<usize>,
    pub max_subagent_depth: Option<usize>,
    pub subagent_model_selection: Option<bool>,
    pub browser_server_url: Option<String>,
    pub browser_server_token: Option<String>,
    pub search_engine: Option<String>,
    #[serde(default = "default_shortcuts")]
    pub shortcuts: HashMap<String, String>,
    pub layout: Option<LayoutNode>,
}

impl Default for TcodeConfig {
    fn default() -> Self {
        Self {
            provider: None,
            api_key: None,
            model: None,
            base_url: None,
            subagent_max_iterations: None,
            max_subagent_depth: None,
            subagent_model_selection: None,
            browser_server_url: None,
            browser_server_token: None,
            search_engine: None,
            shortcuts: default_shortcuts(),
            layout: None,
        }
    }
}

impl TcodeConfig {
    /// Get search engine string, defaulting to "google"
    pub fn search_engine_str(&self) -> &str {
        self.search_engine.as_deref().unwrap_or("google")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

impl fmt::Display for SplitDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SplitDirection::Horizontal => write!(f, "horizontal"),
            SplitDirection::Vertical => write!(f, "vertical"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PanelCommand {
    Display,
    Edit,
    Tree,
    Permission,
}

impl fmt::Display for PanelCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PanelCommand::Display => write!(f, "display"),
            PanelCommand::Edit => write!(f, "edit"),
            PanelCommand::Tree => write!(f, "tree"),
            PanelCommand::Permission => write!(f, "permission"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    Split {
        split: SplitDirection,
        size: Option<u8>,
        a: Box<LayoutNode>,
        b: Box<LayoutNode>,
    },
    Leaf {
        command: PanelCommand,
        size: Option<u8>,
        focus: Option<bool>,
    },
}

impl LayoutNode {
    pub fn default_layout() -> Self {
        LayoutNode::Split {
            split: SplitDirection::Horizontal,
            size: None,
            a: Box::new(LayoutNode::Split {
                split: SplitDirection::Vertical,
                size: Some(70),
                a: Box::new(LayoutNode::Leaf {
                    command: PanelCommand::Display,
                    size: Some(70),
                    focus: None,
                }),
                b: Box::new(LayoutNode::Leaf {
                    command: PanelCommand::Edit,
                    size: Some(30),
                    focus: Some(true),
                }),
            }),
            b: Box::new(LayoutNode::Split {
                split: SplitDirection::Vertical,
                size: Some(30),
                a: Box::new(LayoutNode::Leaf {
                    command: PanelCommand::Tree,
                    size: Some(50),
                    focus: None,
                }),
                b: Box::new(LayoutNode::Leaf {
                    command: PanelCommand::Permission,
                    size: Some(50),
                    focus: None,
                }),
            }),
        }
    }

    pub fn size(&self) -> Option<u8> {
        match self {
            LayoutNode::Split { size, .. } | LayoutNode::Leaf { size, .. } => *size,
        }
    }

    pub fn validate(&self) -> Result<()> {
        let mut display_count = 0;
        let mut focus_count = 0;
        let mut edit_count = 0;
        self.validate_inner(&mut display_count, &mut focus_count, &mut edit_count)?;
        if display_count != 1 {
            bail!("layout must have exactly one display panel, found {display_count}");
        }
        if focus_count > 1 {
            bail!("layout must have at most one focused panel, found {focus_count}");
        }
        if edit_count == 0 {
            bail!("layout must have at least one edit panel");
        }
        Ok(())
    }

    fn validate_inner(
        &self,
        display_count: &mut usize,
        focus_count: &mut usize,
        edit_count: &mut usize,
    ) -> Result<()> {
        match self {
            LayoutNode::Leaf {
                command,
                size,
                focus,
            } => {
                if *command == PanelCommand::Display {
                    *display_count += 1;
                }
                if *command == PanelCommand::Edit {
                    *edit_count += 1;
                }
                if focus == &Some(true) {
                    *focus_count += 1;
                }
                if let Some(s) = size
                    && (*s == 0 || *s > 99)
                {
                    bail!("size must be 1..=99, got {s}");
                }
            }
            LayoutNode::Split { size, a, b, .. } => {
                if let Some(s) = size
                    && (*s == 0 || *s > 99)
                {
                    bail!("size must be 1..=99, got {s}");
                }
                let a_size = a.size();
                let b_size = b.size();
                if let (Some(a_s), Some(b_s)) = (a_size, b_size) {
                    let sum = a_s as u16 + b_s as u16;
                    if sum != 100 {
                        bail!(
                            "sibling sizes must add up to 100, got {} + {} = {}",
                            a_s,
                            b_s,
                            sum
                        );
                    }
                }
                a.validate_inner(display_count, focus_count, edit_count)?;
                b.validate_inner(display_count, focus_count, edit_count)?;
            }
        }
        Ok(())
    }
}

// Custom deserialization for LayoutNode using an intermediate RawLayoutNode

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLayoutNode {
    split: Option<SplitDirection>,
    command: Option<PanelCommand>,
    size: Option<u8>,
    focus: Option<bool>,
    a: Option<Box<RawLayoutNode>>,
    b: Option<Box<RawLayoutNode>>,
}

impl TryFrom<RawLayoutNode> for LayoutNode {
    type Error = String;

    fn try_from(raw: RawLayoutNode) -> std::result::Result<Self, Self::Error> {
        match (raw.split, raw.command) {
            (Some(split), None) => {
                let a = raw.a.ok_or("split node requires child 'a'")?;
                let b = raw.b.ok_or("split node requires child 'b'")?;
                if raw.focus.is_some() {
                    return Err("split node cannot have 'focus'".to_string());
                }
                Ok(LayoutNode::Split {
                    split,
                    size: raw.size,
                    a: Box::new(LayoutNode::try_from(*a)?),
                    b: Box::new(LayoutNode::try_from(*b)?),
                })
            }
            (None, Some(command)) => {
                if raw.a.is_some() || raw.b.is_some() {
                    return Err("leaf node cannot have children 'a' or 'b'".to_string());
                }
                Ok(LayoutNode::Leaf {
                    command,
                    size: raw.size,
                    focus: raw.focus,
                })
            }
            (Some(_), Some(_)) => Err("node cannot have both 'split' and 'command'".to_string()),
            (None, None) => Err("node must have either 'split' or 'command'".to_string()),
        }
    }
}

impl<'de> Deserialize<'de> for LayoutNode {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawLayoutNode::deserialize(deserializer)?;
        LayoutNode::try_from(raw).map_err(serde::de::Error::custom)
    }
}

fn default_shortcuts() -> HashMap<String, String> {
    HashMap::from([
        ("brainstorm".to_string(), "This is a brainstorm to get the requirements and features more clear. Do not implement anything. Ask me questions if there is anything not clear".to_string()),
        ("plan".to_string(), "Design and plan first. Do not implement or change any code before I confirm. Ask me questions if there is anything not clear. Break it into multiple steps if necessary. Do not need to include implementation details like what exact code to add or replace (but can include the important code if it makes sense to be in plan/design doc.)".to_string()),
        ("save-plan".to_string(), "Save the plan to `plan.md`. Include all the details so that it can be used for implementation in a fresh LLM session. Do not need to include implementation details like what exact code to add or replace (but can include the important code if it makes sense to be in plan/design doc.)".to_string()),
        ("implement-plan".to_string(), "Implement plan.md. Ask me questions if there is anything not clear. Use subagent to implement each step if needed, so that you keep your context window clean for large changes and can supervise the overall correctness.".to_string()),
        ("review".to_string(), "Use a subagent to review the change. Only include enough info for the subagent to understand the context. Focus on correctness, edge cases, potential bugs, security, code cleanliness, and dead code. Do not need to pass changes to subagent, it can use git to figure out the changes.".to_string()),
    ])
}

pub(crate) const DEFAULT_CONFIG_TEMPLATE: &str = r#"# tcode configuration
# Uncomment and modify values as needed.

# provider = "claude"              # REQUIRED. one of: claude | claude-oauth | open-ai | open-router
# api_key = ""                     # optional. Empty and omitted behave the same: both fall back to the provider env var, then to "" (no auth) if it is unset. Ignored when provider = "claude-oauth".
# model = "claude-opus-4-6"        # defaults per provider
# base_url = ""                    # defaults per provider
# subagent_max_iterations = 50
# max_subagent_depth = 10
# subagent_model_selection = false
# browser_server_url = ""
# browser_server_token = ""
# search_engine = "google"         # kagi | google

[shortcuts]
brainstorm = """\
  This is a brainstorm to get the requirements and features more clear. \
  Do not implement anything. \
  Ask me questions if there is anything not clear"""
plan = """\
  Design and plan first. Do not implement or change any code before I confirm. \
  Ask me questions if there is anything not clear. \
  Break it into multiple steps if necessary. \
  Do not need to include implementation details like what exact code to add or replace \
  (but can include the important code if it makes sense to be in plan/design doc.)"""
save-plan = """\
  Save the plan to `plan.md`. \
  Include all the details so that it can be used for implementation in a fresh LLM session. \
  Do not need to include implementation details like what exact code to add or replace \
  (but can include the important code if it makes sense to be in plan/design doc.)"""
implement-plan = """\
  Implement plan.md. \
  Ask me questions if there is anything not clear. \
  Use subagent to implement each step if needed, so that you keep your context window \
  clean for large changes and can supervise the overall correctness."""
review = """\
  Use a subagent to review the change. \
  Only include enough info for the subagent to understand the context. \
  Focus on correctness, edge cases, potential bugs, security, code cleanliness, and dead code. \
  Do not need to pass changes to subagent, it can use git to figure out the changes."""

# [layout]
# split = "horizontal"
#
#   [layout.a]
#   split = "vertical"
#   size = 70
#
#     [layout.a.a]
#     command = "display"
#     size = 70
#
#     [layout.a.b]
#     command = "edit"
#     size = 30
#     focus = true
#
#   [layout.b]
#   split = "vertical"
#   size = 30
#
#     [layout.b.a]
#     command = "tree"
#     size = 50
#
#     [layout.b.b]
#     command = "permission"
#     size = 50
"#;

pub fn config_path_for(profile: Option<&str>) -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    let dir = home.join(".tcode");
    let filename = match profile {
        Some(p) => format!("config-{p}.toml"),
        None => "config.toml".to_string(),
    };
    Ok(dir.join(filename))
}

pub fn config_file_exists(profile: Option<&str>) -> bool {
    config_path_for(profile)
        .map(|p| p.exists())
        .unwrap_or(false)
}

pub fn load_config(profile: Option<&str>) -> Result<TcodeConfig> {
    let path = config_path_for(profile)?;

    if !path.exists() {
        match profile {
            Some(p) => bail!(
                "config not found at {}. Run `tcode -p {} config` to create it.",
                path.display(),
                p
            ),
            None => bail!(
                "config not found at {}. Run `tcode config` to create it.",
                path.display()
            ),
        }
    }

    let contents = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;

    let config: TcodeConfig = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;

    if let Some(ref layout) = config.layout {
        layout
            .validate()
            .map_err(|e| anyhow::anyhow!("invalid [layout] in {}: {e}", path.display()))?;
    }

    Ok(config)
}
