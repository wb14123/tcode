-- tcode Shortcut Templates
-- Copy this file to ~/.tcode/shortcuts.lua and customize.
--
-- Usage: In the edit buffer, type /shortcutname and press <Tab> to expand.
-- Type / or a partial name + <Tab> to see matching shortcuts in a popup.
--
-- Each key is the shortcut name (used as /name), value is the expanded text.
-- Use [[...]] for multi-line templates.
-- For names with hyphens, quote the key: ["my-name"] = "..."

return {
  plan = [[Design and plan first. Do not implement or change any code before I confirm. Ask me questions if there is anything not clear.]],
  ["save-plan"] = [[Save the plan to `plan.md`. Include all the details so that it can be used for implementation in a fresh LLM session.]],
  ["implement-plan"] = [[Implement plan.md . Ask me questions if there is anything not clear.]],
  review = [[Use a subagent to review the change. Only include enough info for the subagent to understand the context. Focus on correctness, edge cases, potential bugs, security, and code cleanliness. Do not need to pass changes to subagent, it can use git to figure out the changes.]],
}
