use serde::Deserialize;

/// Project-level configuration loaded from `<cwd>/.tcode/config.toml`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct ProjectConfig {
    /// Container name to use for the project.
    pub(crate) container: Option<String>,

    /// Container runtime executable (e.g. "docker", "podman").
    pub(crate) container_runtime: Option<String>,
}

/// Load the project configuration from the current working directory.
///
/// Looks for `.tcode/config.toml` in the current working directory.
/// Returns `None` if the file does not exist or cannot be parsed.
pub(crate) fn load() -> Option<ProjectConfig> {
    let cwd = std::env::current_dir().ok()?;
    let path = cwd.join(".tcode").join("config.toml");
    let contents = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&contents).ok()
}
