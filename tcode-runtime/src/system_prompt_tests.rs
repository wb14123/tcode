#[cfg(test)]
mod tests {
    use llm_rs::conversation::SystemPromptContext;

    use crate::session::SessionMode;
    use llm_rs::tool::ContainerConfig;

    use crate::system_prompt::tcode_system_prompt_builder;

    fn build_prompt(
        session_mode: SessionMode,
        container_config: Option<ContainerConfig>,
    ) -> String {
        let builder = tcode_system_prompt_builder(session_mode, container_config);
        builder(SystemPromptContext { subagent_depth: 0 })
    }

    #[test]
    fn normal_system_prompt_keeps_current_directory_context() {
        let prompt = build_prompt(SessionMode::Normal, None);

        assert!(prompt.contains("Current directory:"));
        assert!(prompt.contains("Subagent Rules"));
        assert!(prompt.contains("Tool Usage"));
        assert!(prompt.contains("`read`/`write`/`edit`"));
    }

    #[test]
    fn normal_system_prompt_includes_container_guidance_when_configured() {
        let prompt = build_prompt(
            SessionMode::Normal,
            Some(ContainerConfig {
                name: "test-container".to_string(),
                runtime: "docker".to_string(),
                uid: 1000,
                gid: 1000,
                home: "/home/test".to_string(),
            }),
        );

        assert!(prompt.contains("## Container Mode"));
        assert!(prompt.contains("test-container"));
        assert!(prompt.contains("docker"));
    }

    #[test]
    fn web_only_system_prompt_omits_local_context_and_disabled_tool_guidance() {
        let prompt = build_prompt(
            SessionMode::WebOnly,
            Some(ContainerConfig {
                name: "test-container".to_string(),
                runtime: "docker".to_string(),
                uid: 1000,
                gid: 1000,
                home: "/home/test".to_string(),
            }),
        );

        assert!(!prompt.contains("Current directory:"));
        assert!(!prompt.contains("CLAUDE.md"));
        assert!(!prompt.contains("Container Mode"));
        assert!(!prompt.contains("bash"));
        assert!(!prompt.contains("shell"));
        assert!(!prompt.contains("LSP"));
        for tool_name in ["`read`", "`write`", "`edit`", "`grep`", "`glob`"] {
            assert!(!prompt.contains(tool_name), "prompt mentions {tool_name}");
        }
        for tool_name in [
            "`current_time`",
            "`web_search`",
            "`web_fetch`",
            "`subagent`",
            "`continue_subagent`",
        ] {
            assert!(prompt.contains(tool_name), "prompt omits {tool_name}");
        }
    }

    #[test]
    fn subagent_depth_selects_subagent_role() {
        let builder = tcode_system_prompt_builder(SessionMode::WebOnly, None);
        let prompt = builder(SystemPromptContext { subagent_depth: 1 });

        assert!(prompt.starts_with("You are a subagent spawned for a specific task."));
        assert!(prompt.contains("This session is web-only"));
    }
}
