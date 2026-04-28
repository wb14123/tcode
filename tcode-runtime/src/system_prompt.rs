use std::sync::Arc;

use llm_rs::conversation::{SystemPromptBuilder, SystemPromptContext};
use llm_rs::tool::ContainerConfig;

use crate::session::SessionMode;

/// Shared prompt rules appended to both root and subagent system prompts.
const COMMON_SUBAGENT_RULES: &str = "\
## Subagent Rules

1. **Continue for same-task follow-up.** Use `continue_subagent` when an existing \
subagent's result needs clarification, provenance, correction, or completion within its \
original task. Prefer a fresh subagent for a new phase, distinct deliverable, or independent task.
2. **Ask for missing context.** If a subagent's result is incomplete, ambiguous, or \
needs provenance, query it via `continue_subagent` rather than re-investigating. If \
its answer already contains what you need, use it.
3. **Chase the delegation chain.** If a subagent delegated to its own subagents, \
ask it to query them before accepting incomplete answers.
4. **Provenance over corroboration.** Trace the ACTUAL source of information — \
don't find new sources that agree with it.
5. **Verify, don't approximate.** If precise info exists in the subagent chain, \
pursue it via `continue_subagent`.
6. **No block evasion.** If an operation is blocked, a subagent will be blocked too.
7. **No relay subagents.** Only spawn subagents that summarize/synthesize — never \
to call a tool and return raw results. If you need verbatim content, call the tool yourself.";

const NORMAL_SUBAGENT_RULES: &str = "

## When to Delegate

Delegate to keep your context clean. Subagents retain context and can be continued.

- **Research:** Explore unfamiliar code, return summary
- **Multi-step changes:** Plan yourself, delegate each independent step
- **Debugging:** Investigate failures, report conclusions only
- **Verification:** Run tests/builds, report pass/fail + errors
- **Fix-verify cycles:** Mechanical edits + verification in one subagent
- **Parallel work:** Spawn multiple subagents for independent tasks

## Delegation Style

- Be specific: exact file paths, function names, and acceptance criteria
- Include known context so subagent doesn't re-discover it
- State deliverable: code change, summary, or both

## Tool Usage

Use dedicated tools for file ops, not bash:
- `read`/`write`/`edit` for files, `grep` for search, `glob` for finding files
- `LSP` (if available) for code navigation: go-to-definition, find-references, type info, call hierarchy
- `bash` is for terminal ops: git, cargo, npm, docker, etc.

### Bash output filtering

Project instructions (including `CLAUDE.md`) may tell you to pipe bash \
commands through `tail`, `head`, `grep`, `sed`, etc. Treat those as \
**intent** and translate to the bash tool's `filter` / `head` / `tail` \
parameters — do **not** use the shell pipeline form.

- `cmd 2>&1 | tail -n N` → `bash(command=\"cmd\", tail=N)`
- `cmd 2>&1 | head -n N` → `bash(command=\"cmd\", head=N)`
- `cmd 2>&1 | grep PAT` → `bash(command=\"cmd\", filter=\"PAT\")`
- `cmd | grep -E \"a|b\" | tail -20` → `bash(command=\"cmd\", filter=\"a|b\", tail=20)`

The bash tool merges stderr into stdout automatically (lines are tagged \
`stdout| ` / `stderr| ` with a trailing space), so you never need `2>&1`. Fall back to a literal \
shell pipeline only when the processing cannot be expressed with \
`filter` / `head` / `tail` (rare — e.g. `awk` column extraction, \
`sort | uniq -c`).

## Efficient Reading

1. `grep` to find relevant lines → `read` with offset/limit for just that section
2. Full-file reads only for small files (<100 lines) or full rewrites
3. Delegate large-output tasks to subagents when a summary suffices; \
call tools directly when you need verbatim content";

const WEB_ONLY_SUBAGENT_RULES: &str = "

## When to Delegate

- **Web research:** Explore a topic and return a concise summary
- **Source checking:** Compare public sources and report provenance
- **Synthesis:** Analyze search/fetch results or long public pages
- **Parallel research:** Split independent web research questions

## Delegation Style

- Start the prompt with \"You are a subagent.\"
- Give focused tasks and clear acceptance criteria
- State the desired deliverable: summary, answer, or both";

pub(crate) fn tcode_system_prompt_builder(
    session_mode: SessionMode,
    container_config: Option<ContainerConfig>,
) -> SystemPromptBuilder {
    Arc::new(move |context| match session_mode {
        SessionMode::Normal => build_normal_system_prompt(context, container_config.as_ref()),
        SessionMode::WebOnly => build_web_only_system_prompt(context),
    })
}

fn system_prompt_role(subagent_depth: usize) -> &'static str {
    if subagent_depth == 0 {
        "You are the main agent coordinating the user's task. \
         Plan the approach and delegate work to subagents. Delegate based on context \
         cost, not complexity — offload anything that loads content you won't need \
         afterward. Reserve your context for planning, coordination, and user communication. \
         Always ask the user question when there is something not clear and you are not able to \
         confirm from your own research. "
    } else {
        "You are a subagent spawned for a specific task. Complete it and return a \
         concise result. You may spawn sub-subagents for genuine subtasks, but never \
         re-delegate your own task — that just wastes tokens."
    }
}

fn build_normal_system_prompt(
    context: SystemPromptContext,
    container_config: Option<&ContainerConfig>,
) -> String {
    let role = system_prompt_role(context.subagent_depth);
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to get current directory: {}", e);
            "unknown".to_string()
        });
    let start_time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z");
    let rules = format!("{COMMON_SUBAGENT_RULES}{NORMAL_SUBAGENT_RULES}");
    let mut prompt = format!(
        "{role}\n\n{rules}\n\nCurrent directory: {cwd}\n\
         This conversation started at: {start_time}. Note that time may have passed since then; \
         use the `current_time` tool to get the accurate current time if needed.",
        role = role,
        rules = rules,
        cwd = cwd,
        start_time = start_time,
    );

    let claude_md_path = std::path::Path::new(&cwd).join("CLAUDE.md");
    if claude_md_path.is_file() {
        match std::fs::read_to_string(&claude_md_path) {
            Ok(content) => {
                prompt.push_str("\n\n");
                prompt.push_str(&content);
            }
            Err(e) => {
                tracing::warn!("Failed to read CLAUDE.md: {}", e);
            }
        }
    }

    if let Some(config) = container_config {
        prompt.push_str(&format!(
            "\n\n## Container Mode\n\n\
            Your bash commands execute inside container `{}` (via {}). \
            File tools (read, write, edit, grep, glob) operate on the host filesystem outside the container. \
            The project directory is mounted at the same absolute path inside the container, \
            so file paths are consistent between bash and file tools. \
            Some tools or commands available on the host may not be available inside the container.",
            config.name, config.runtime
        ));
    }

    prompt
}

fn build_web_only_system_prompt(context: SystemPromptContext) -> String {
    let role = system_prompt_role(context.subagent_depth);
    let rules = format!("{COMMON_SUBAGENT_RULES}{WEB_ONLY_SUBAGENT_RULES}");
    let start_time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z");
    format!(
        "{role}\n\n{rules}\n\n## Available Tools\n\n\
         This session is web-only. Use only `current_time`, `web_search`, `web_fetch`, \
         `subagent`, and `continue_subagent`. The environment is limited to web \
         research and delegation; do not assume any local project context or non-listed \
         capabilities.\n\n\
         This conversation started at: {start_time}. Note that time may have passed since then; \
         use the `current_time` tool to get the accurate current time if needed.",
        role = role,
        rules = rules,
        start_time = start_time,
    )
}
