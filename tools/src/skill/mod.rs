//! Skill tool: load specialized skill instruction packs by name.

use std::sync::Arc;

use llm_rs::skill::{self, SkillMeta};
use llm_rs::tool::Tool;

/// Build the tool description based on available skills.
fn build_description(skills: &[SkillMeta]) -> String {
    if skills.is_empty() {
        return "Load a specialized skill by name. There are currently no skills available. Do not use this tool.".to_string();
    }

    let mut desc = String::from(
        "Load a specialized skill that provides domain-specific instructions and workflows. \
         Skills are reusable \"instruction packs\" loaded from SKILL.md files.\n\n\
         Available skills:\n",
    );
    for s in skills {
        desc.push_str(&skill::format_skill_entry(s));
        desc.push('\n');
    }
    desc.push_str(
        "\nWhen to load a skill:\n\
         - When the user explicitly asks for it\n\
         - When the task matches a skill's description or \"when to use\" guidance\n\n\
         IMPORTANT: Only load skills listed above. Do not guess skill names. \
         If no skills are listed, do not use this tool.",
    );
    desc
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct SkillParams {
    /// Name of the skill to load (must be from the available skills list)
    name: String,
}

/// Create the Skill tool with the given available skills.
pub fn skill_tool(skills: Arc<Vec<SkillMeta>>) -> Tool {
    let description = build_description(&skills);
    let skills_clone = Arc::clone(&skills);

    Tool::new(
        "skill",
        description,
        None,
        move |_ctx: llm_rs::tool::ToolContext, params: SkillParams| {
            let skills = Arc::clone(&skills_clone);
            async_stream::stream! {
                let skill = skills.iter().find(|s| s.name == params.name);
                let skill = match skill {
                    Some(s) => s,
                    None => {
                        let available: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
                        yield Err(format!(
                            "Unknown skill '{}'. Available skills: {}",
                            params.name,
                            available.join(", ")
                        ));
                        return;
                    }
                };

                let content = match skill::load_skill_content(skill) {
                    Ok(c) => c,
                    Err(e) => {
                        yield Err(format!("Failed to load skill '{}': {}", params.name, e));
                        return;
                    }
                };

                let files = skill::list_skill_files(skill);

                let mut output = format!(
                    "<skill_content name=\"{}\">\n# Skill: {}\n\n{}\n\nBase directory: {}",
                    skill.name,
                    skill.name,
                    content,
                    skill.dir.display()
                );

                if !files.is_empty() {
                    output.push_str("\n\n<skill_files>");
                    for file in &files {
                        output.push_str(&format!("\n<file>{}</file>", file.display()));
                    }
                    output.push_str("\n</skill_files>");
                }

                output.push_str("\n</skill_content>");

                yield Ok(output);
            }
        },
    )
}
