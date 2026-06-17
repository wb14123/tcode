use crate::project_config::ProjectConfig;

#[test]
fn parse_config_with_container() {
    let cfg: ProjectConfig = toml::from_str(r#"container = "my-container""#).unwrap();
    assert_eq!(cfg.container.as_deref(), Some("my-container"));
    assert_eq!(cfg.container_runtime, None);
}

#[test]
fn parse_config_with_container_and_runtime() {
    let cfg: ProjectConfig = toml::from_str(
        r#"
container = "my-container"
container_runtime = "podman"
"#,
    )
    .unwrap();
    assert_eq!(cfg.container.as_deref(), Some("my-container"));
    assert_eq!(cfg.container_runtime.as_deref(), Some("podman"));
}

#[test]
fn parse_config_empty() {
    let cfg: ProjectConfig = toml::from_str("").unwrap();
    assert_eq!(cfg.container, None);
    assert_eq!(cfg.container_runtime, None);
}

#[test]
fn parse_config_unknown_field_is_ok() {
    let cfg: ProjectConfig = toml::from_str(
        r#"
container = "my-container"
unknown_field = "ignored"
"#,
    )
    .unwrap();
    assert_eq!(cfg.container.as_deref(), Some("my-container"));
}

#[test]
fn parse_config_with_table_section_fails() {
    let result: Result<ProjectConfig, _> = toml::from_str(
        r#"[container]
name = "my-container"
"#,
    );
    assert!(
        result.is_err(),
        "table section [container] should fail to parse"
    );
}
