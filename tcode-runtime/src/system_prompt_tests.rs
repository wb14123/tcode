#[cfg(test)]
mod tests {
    use llm_rs::conversation::SystemPromptContext;

    use crate::session::SessionMode;
    use llm_rs::tool::ContainerConfig;

    use crate::system_prompt::{project_instructions, tcode_system_prompt_builder};
    use crate::test_support::TestDir;

    fn build_prompt(
        session_mode: SessionMode,
        container_config: Option<ContainerConfig>,
    ) -> String {
        let builder = tcode_system_prompt_builder(session_mode, container_config);
        builder(SystemPromptContext { subagent_depth: 0 }).unwrap()
    }

    #[test]
    fn normal_system_prompt_keeps_current_directory_context() {
        let prompt = build_prompt(SessionMode::Normal, None);

        assert!(prompt.contains("Current directory:"));
        assert!(prompt.contains("Subagent Rules"));
        assert!(prompt.contains("Tool Usage"));
        assert!(prompt.contains("Strongly prefer the dedicated file tools"));
        assert!(prompt.contains("Output Style"));
    }

    #[test]
    fn normal_system_prompt_prefers_file_tools_over_bash() {
        let prompt = build_prompt(SessionMode::Normal, None);

        assert!(prompt.contains("Never use bash to read code or files"));
        assert!(prompt.contains("`cat`, `ls`, `find`, `grep`/`rg`"));
        assert!(prompt.contains("auto-reviewed"));
    }

    #[test]
    fn normal_system_prompt_prefers_simple_commands() {
        let prompt = build_prompt(SessionMode::Normal, None);

        assert!(prompt.contains("Prefer simple commands"));
        assert!(prompt.contains("`&&`"));
        assert!(prompt.contains("`;`"));
        assert!(prompt.contains("allowlist"));
        assert!(prompt.contains("exit code is already returned"));
    }

    #[test]
    fn subagent_prompt_includes_file_tool_preference() -> anyhow::Result<()> {
        let builder = tcode_system_prompt_builder(SessionMode::Normal, None);
        let prompt = builder(SystemPromptContext { subagent_depth: 1 })?;

        assert!(prompt.starts_with("You are a subagent spawned for a specific task."));
        assert!(prompt.contains("Never use bash to read code or files"));
        assert!(prompt.contains("Strongly prefer the dedicated file tools"));
        assert!(prompt.contains("Prefer simple commands"));
        Ok(())
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
        assert!(!prompt.contains("AGENTS.md"));
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
        assert!(prompt.contains("Output Style"));
    }

    #[test]
    fn subagent_depth_selects_subagent_role() -> anyhow::Result<()> {
        let builder = tcode_system_prompt_builder(SessionMode::WebOnly, None);
        let prompt = builder(SystemPromptContext { subagent_depth: 1 })?;

        assert!(prompt.starts_with("You are a subagent spawned for a specific task."));
        assert!(prompt.contains("This session is web-only"));
        Ok(())
    }

    // ======== project_instructions ========

    #[test]
    fn project_instructions_uses_agents_md_when_only_it_exists() -> anyhow::Result<()> {
        let dir = TestDir::new("system_prompt");
        std::fs::write(dir.path().join("AGENTS.md"), "agents content")?;

        let instructions = project_instructions(dir.path())?;
        assert_eq!(instructions.as_deref(), Some("agents content"));
        Ok(())
    }

    #[test]
    fn project_instructions_uses_claude_md_when_only_it_exists() -> anyhow::Result<()> {
        let dir = TestDir::new("system_prompt");
        std::fs::write(dir.path().join("CLAUDE.md"), "claude content")?;

        let instructions = project_instructions(dir.path())?;
        assert_eq!(instructions.as_deref(), Some("claude content"));
        Ok(())
    }

    #[test]
    fn project_instructions_prefers_agents_md_over_claude_md() -> anyhow::Result<()> {
        let dir = TestDir::new("system_prompt");
        std::fs::write(dir.path().join("AGENTS.md"), "agents content")?;
        std::fs::write(dir.path().join("CLAUDE.md"), "claude content")?;

        let instructions =
            project_instructions(dir.path())?.expect("instructions present when AGENTS.md exists");
        assert!(instructions.contains("agents content"));
        assert!(!instructions.contains("claude content"));
        Ok(())
    }

    #[test]
    fn project_instructions_returns_none_when_neither_file_exists() -> anyhow::Result<()> {
        let dir = TestDir::new("system_prompt");

        let instructions = project_instructions(dir.path())?;
        assert!(instructions.is_none());
        Ok(())
    }

    #[test]
    fn project_instructions_fails_on_invalid_utf8_agents_md_without_fallback() -> anyhow::Result<()>
    {
        let dir = TestDir::new("system_prompt");
        std::fs::write(dir.path().join("AGENTS.md"), [0xff, 0xfe, 0x80, 0x00])?;
        std::fs::write(dir.path().join("CLAUDE.md"), "claude content")?;

        let err = project_instructions(dir.path()).unwrap_err();
        assert!(err.to_string().contains("AGENTS.md"), "error: {err}");
        Ok(())
    }

    #[test]
    fn project_instructions_fails_on_invalid_utf8_claude_md_when_agents_md_absent()
    -> anyhow::Result<()> {
        let dir = TestDir::new("system_prompt");
        std::fs::write(dir.path().join("CLAUDE.md"), [0xff, 0xfe, 0x80, 0x00])?;

        let err = project_instructions(dir.path()).unwrap_err();
        assert!(err.to_string().contains("CLAUDE.md"), "error: {err}");
        Ok(())
    }

    #[test]
    fn project_instructions_fails_on_invalid_utf8_agents_md_without_claude_md() -> anyhow::Result<()>
    {
        let dir = TestDir::new("system_prompt");
        std::fs::write(dir.path().join("AGENTS.md"), [0xff, 0xfe, 0x80, 0x00])?;

        let err = project_instructions(dir.path()).unwrap_err();
        assert!(err.to_string().contains("AGENTS.md"), "error: {err}");
        Ok(())
    }
}
