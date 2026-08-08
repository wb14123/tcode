//! Fast pre-filtering of `display.jsonl` lines before a full typed parse.
//!
//! Every consumer of `display.jsonl` (the server's stale-close scan, the
//! tree pane, ...) only reacts to a small subset of the `Message` variants.
//! The vast majority of lines are chunk/thinking/arg-chunk events that every
//! consumer discards. A cheap substring check on the variant-name JSON key
//! (`"VariantName"`) skips those lines without any JSON parsing; only the
//! handful of lines mentioning a relevant variant get a real parse.

/// JSON variant-name keys (with surrounding quotes) that the server's
/// stale-close scan reacts to.
pub const STALE_CLOSE_MARKERS: &[&str] = &[
    "\"ToolMessageStart\"",
    "\"ToolMessageEnd\"",
    "\"SubAgentStart\"",
    "\"SubAgentTurnEnd\"",
    "\"SubAgentContinue\"",
    "\"SubAgentEnd\"",
];

/// JSON variant-name keys that the tree pane's `process_event` reacts to.
pub const TREE_MARKERS: &[&str] = &[
    "\"AssistantToolCallStart\"",
    "\"ToolMessageStart\"",
    "\"ToolMessageEnd\"",
    "\"SubAgentStart\"",
    "\"SubAgentEnd\"",
    "\"SubAgentTurnEnd\"",
    "\"SubAgentContinue\"",
    "\"ToolRequestPermission\"",
    "\"ToolPermissionApproved\"",
    "\"SubAgentWaitingPermission\"",
    "\"SubAgentPermissionApproved\"",
    "\"SubAgentPermissionDenied\"",
    "\"SubAgentInputStart\"",
];

/// True if `line` contains any of the given marker substrings.
///
/// The variant name always appears literally as a JSON key in a line of that
/// variant (in both the envelope and legacy wire formats), so this filter has
/// no false negatives for the variants in `markers`. It also has no false
/// positives: a string value that merely *mentions* a variant name is JSON
/// escaped (`\"VariantName\"`), so the literal quoted marker cannot match it.
pub fn line_mentions_any(line: &str, markers: &[&str]) -> bool {
    markers.iter().any(|m| line.contains(m))
}
