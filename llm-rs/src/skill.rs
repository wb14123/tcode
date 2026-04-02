//! Skill system: discovery, parsing, and loading of SKILL.md instruction packs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// YAML frontmatter fields we extract from SKILL.md.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    when_to_use: Option<String>,
}

/// Where a skill was loaded from (determines priority).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillSource {
    ProjectTcode,  // <cwd>/.tcode/skills/
    ProjectClaude, // <cwd>/.claude/skills/
    UserTcode,     // ~/.tcode/skills/
    UserClaude,    // ~/.claude/skills/
}

impl std::fmt::Display for SkillSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillSource::ProjectTcode => write!(f, ".tcode/skills (project)"),
            SkillSource::ProjectClaude => write!(f, ".claude/skills (project)"),
            SkillSource::UserTcode => write!(f, "~/.tcode/skills (user)"),
            SkillSource::UserClaude => write!(f, "~/.claude/skills (user)"),
        }
    }
}

/// Parsed metadata from a SKILL.md frontmatter.
#[derive(Debug, Clone)]
pub struct SkillMeta {
    /// Skill name (from frontmatter `name`, or directory name).
    pub name: String,
    /// One-liner for listings.
    pub description: Option<String>,
    /// Guidance for LLM auto-invocation.
    pub when_to_use: Option<String>,
    /// Absolute path to the skill directory.
    pub dir: PathBuf,
    /// Absolute path to the SKILL.md file.
    pub skill_file: PathBuf,
    /// Where it was loaded from.
    pub source: SkillSource,
}

/// Scan all 4 skill directories, return deduplicated skills (project-local wins).
/// Returns (skills, warnings) where warnings are duplicate-shadow messages.
pub fn scan_skills() -> (Vec<SkillMeta>, Vec<String>) {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("Failed to get current directory: {e}");
            return (Vec::new(), Vec::new());
        }
    };
    let home = dirs_or_cwd();

    let dirs: Vec<(PathBuf, SkillSource)> = vec![
        (cwd.join(".tcode/skills"), SkillSource::ProjectTcode),
        (cwd.join(".claude/skills"), SkillSource::ProjectClaude),
        (home.join(".tcode/skills"), SkillSource::UserTcode),
        (home.join(".claude/skills"), SkillSource::UserClaude),
    ];

    scan_skills_from_dirs(&dirs)
}

/// Scan the given skill directories in order, return deduplicated skills (first wins).
/// Returns (skills, warnings) where warnings are duplicate-shadow messages.
pub fn scan_skills_from_dirs(dirs: &[(PathBuf, SkillSource)]) -> (Vec<SkillMeta>, Vec<String>) {
    let mut skills = Vec::new();
    let mut warnings = Vec::new();
    let mut seen: HashMap<String, SkillSource> = HashMap::new();

    for (base_dir, source) in dirs {
        if !base_dir.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(base_dir) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("Failed to read skill directory {}: {e}", base_dir.display());
                continue;
            }
        };
        for entry_result in entries {
            let entry = match entry_result {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(
                        "Failed to read directory entry in {}: {e}",
                        base_dir.display()
                    );
                    continue;
                }
            };
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let skill_file = path.join("SKILL.md");
            if !skill_file.is_file() {
                continue;
            }
            let dir_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            match parse_skill_md(&skill_file, &path, dir_name, *source) {
                Ok(meta) => {
                    if let Some(existing_source) = seen.get(&meta.name) {
                        warnings.push(format!(
                            "Skill '{}' from {} skipped: already defined in {}",
                            meta.name, source, existing_source
                        ));
                    } else {
                        seen.insert(meta.name.clone(), *source);
                        skills.push(meta);
                    }
                }
                Err(e) => {
                    warnings.push(format!("Failed to parse {}: {}", skill_file.display(), e));
                }
            }
        }
    }

    (skills, warnings)
}

fn dirs_or_cwd() -> PathBuf {
    home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Sanitize a skill name from frontmatter: strip control chars, trim, cap at 100 chars.
fn sanitize_skill_name(name: &str) -> String {
    let sanitized: String = name.chars().filter(|c| !c.is_control()).collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        return String::new(); // caller will use dir_name fallback
    }
    if trimmed.len() > 100 {
        let boundary = trimmed.floor_char_boundary(100);
        trimmed[..boundary].to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parse a single SKILL.md file, extract frontmatter fields.
fn parse_skill_md(
    path: &Path,
    dir: &Path,
    dir_name: &str,
    source: SkillSource,
) -> Result<SkillMeta> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let content = content.replace("\r\n", "\n");

    let (name, description, when_to_use) = if let Some(after_open) = content.strip_prefix("---\n") {
        // Find the closing delimiter
        if let Some(end) = after_open.find("\n---") {
            let yaml_str = &after_open[..end];
            let fm: Frontmatter = match serde_saphyr::from_str(yaml_str) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("Failed to parse YAML frontmatter: {e}");
                    Frontmatter::default()
                }
            };

            let name = fm
                .name
                .as_deref()
                .map(sanitize_skill_name)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| dir_name.to_string());
            let description = fm.description;
            let when_to_use = fm.when_to_use;

            (name, description, when_to_use)
        } else {
            (dir_name.to_string(), None, None)
        }
    } else {
        (dir_name.to_string(), None, None)
    };

    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let skill_file = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    Ok(SkillMeta {
        name,
        description,
        when_to_use,
        dir,
        skill_file,
        source,
    })
}

/// Load the full content of a skill for the Skill tool response.
/// Returns the full content (including frontmatter), with `${CLAUDE_SKILL_DIR}` substituted.
pub fn load_skill_content(skill: &SkillMeta) -> Result<String> {
    let content = std::fs::read_to_string(&skill.skill_file)
        .with_context(|| format!("reading {}", skill.skill_file.display()))?;
    let dir_str = skill.dir.to_string_lossy();
    Ok(content.replace("${CLAUDE_SKILL_DIR}", &dir_str))
}

/// List up to 10 non-SKILL.md files (shallow, no recursion) in the skill directory.
pub fn list_skill_files(skill: &SkillMeta) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(&skill.dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                "Failed to read skill directory {}: {e}",
                skill.dir.display()
            );
            return Vec::new();
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry_result| match entry_result {
            Ok(entry) => {
                let path = entry.path();
                if !path.is_file() {
                    return None;
                }
                if path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md") {
                    return None;
                }
                Some(path)
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to read directory entry in {}: {e}",
                    skill.dir.display()
                );
                None
            }
        })
        .collect();
    files.sort();
    files.truncate(10);
    files
}

/// Format a single skill entry line, capped at 250 bytes (truncated at a UTF-8 safe boundary).
pub fn format_skill_entry(skill: &SkillMeta) -> String {
    let mut entry = format!("- {}", skill.name);
    if let Some(ref desc) = skill.description {
        entry.push_str(": ");
        entry.push_str(desc);
    }
    if let Some(ref when) = skill.when_to_use {
        entry.push_str(" - ");
        entry.push_str(when);
    }
    if entry.len() > 250 {
        let boundary = entry.floor_char_boundary(250);
        entry.truncate(boundary);
        entry.push('\u{2026}'); // …
    }
    entry
}
