-- tcode Shortcut Templates
-- Copy this file to ~/.tcode/shortcuts.lua and customize.
--
-- Usage: In the edit buffer, type /shortcutname and press <Tab> to expand.
-- Type / or a partial name + <Tab> to see matching shortcuts in a popup.
--
-- Each key is the shortcut name (used as /name), value is the expanded text.
-- Use [[...]] for multi-line templates.

return {
	brainstorm = "This is a brainstorm to get the requirements and features more clear. "
		.. "Do not implement anything. "
		.. "Ask me questions if there is anything not clear",
	plan = "Design and plan first. Do not implement or change any code before I confirm. "
		.. "Ask me questions if there is anything not clear. "
		.. "Break it into multiple steps if necessary. "
		.. "Do not need to include implementation details like what exact code to add or replace "
		.. "(but can include the important code if it makes sense to be in plan/design doc.)",
	["save-plan"] = "Save the plan to `plan.md`. "
		.. "Include all the details so that it can be used for implementation in a fresh LLM session. "
		.. "Do not need to include implementation details like what exact code to add or replace "
		.. "(but can include the important code if it makes sense to be in plan/design doc.)",
	["implement-plan"] = "Implement plan.md. "
		.. "Ask me questions if there is anything not clear. "
		.. "Use subagent to implement each step if needed, so that you keep your context window "
		.. "clean for large changes and can supervise the overall correctness.",
	review = "Use a subagent to review the change. "
		.. "Only include enough info for the subagent to understand the context. "
		.. "Focus on correctness, edge cases, potential bugs, security, code cleanliness, and dead code. "
		.. "Do not need to pass changes to subagent, it can use git to figure out the changes.",
}
