//! Branching a conversation off at a user message.
//!
//! `run_branch` clones a session's history strictly before a target user
//! message into a brand-new independent session and opens it in a new tmux
//! tab. The source session is never modified.

use std::collections::HashSet;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use llm_rs::conversation::{BroadcastMessage, ConversationState, Message};
use llm_rs::llm::LLMMessage;
use llm_rs::media::ContentPart;
use rand::RngExt;

/// Result of cutting a display file at a target user-message envelope.
#[derive(Debug)]
pub(crate) struct DisplayCut {
    /// Raw display lines strictly before the target line (verbatim).
    pub(crate) retained_lines: Vec<String>,
    /// Typed-parsed envelopes from the retained lines.
    pub(crate) retained_envelopes: Vec<BroadcastMessage>,
    /// Top-level `id` of every retained line that has one, in file order.
    pub(crate) retained_ids: Vec<i32>,
    /// Largest retained id (`None` when there are none).
    pub(crate) max_retained_id: Option<i32>,
    /// 1-based position of the target among `UserMessage` envelopes.
    pub(crate) target_ordinal: usize,
    /// The target `UserMessage` envelope's message.
    pub(crate) target: Message,
    /// The target envelope's top-level id (equals the requested `msg_id`).
    pub(crate) target_id: i32,
}

/// Convert a JSON number to i32, tolerating i64/u64 forms and integral floats.
pub(crate) fn number_to_i32(n: &serde_json::Number) -> Option<i32> {
    if let Some(v) = n.as_i64() {
        return i32::try_from(v).ok();
    }
    if let Some(v) = n.as_u64() {
        return i32::try_from(v).ok();
    }
    if let Some(f) = n.as_f64()
        && f.fract() == 0.0
        && f >= i32::MIN as f64
        && f <= i32::MAX as f64
    {
        return Some(f as i32);
    }
    None
}

/// Index of the ordinal-th (1-based) `LLMMessage::User` in `llm_msgs`, or
/// `None` when `ordinal` is 0 or exceeds the number of user messages.
fn nth_user_msg_index(llm_msgs: &[LLMMessage], ordinal: usize) -> Option<usize> {
    let mut seen = 0usize;
    for (i, msg) in llm_msgs.iter().enumerate() {
        if matches!(msg, LLMMessage::User(_)) {
            seen += 1;
            if seen == ordinal {
                return Some(i);
            }
        }
    }
    None
}

/// Truncate `llm_msgs` to everything strictly before the ordinal-th (1-based)
/// user message. `LLMMessage::System` at index 0 is always kept.
pub(crate) fn truncate_state_at_user(
    state: ConversationState,
    ordinal: usize,
) -> Result<ConversationState> {
    let user_count = state
        .llm_msgs
        .iter()
        .filter(|m| matches!(m, LLMMessage::User(_)))
        .count();
    if ordinal == 0 || ordinal > user_count {
        bail!(
            "cannot truncate conversation state at user message ordinal {ordinal}: state has {user_count} user message(s)"
        );
    }
    let cut = nth_user_msg_index(&state.llm_msgs, ordinal).expect("ordinal bounds checked above");
    let mut state = state;
    state.llm_msgs.truncate(cut);
    Ok(state)
}

/// Cut a display file at the `UserMessage` envelope whose top-level `id`
/// equals `target_msg_id`.
///
/// Lines are parsed one at a time: unparseable lines (a partial line being
/// appended concurrently) are warned about and skipped, lines without a
/// top-level `id` and without a variant-level `msg_id` are skipped, and a
/// legacy line (variant-level `msg_id` only) is a hard "old format" error.
pub(crate) fn truncate_display_at_msg_id(
    lines: &[String],
    target_msg_id: i32,
) -> Result<DisplayCut> {
    let mut retained_envelopes: Vec<BroadcastMessage> = vec![];
    let mut retained_ids: Vec<i32> = vec![];
    let mut user_msg_count = 0usize;
    let mut target_ordinal = 0usize;
    let mut target: Option<Message> = None;
    let mut cut_line: Option<usize> = None;

    for (line_idx, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    line = line_idx + 1,
                    "skipping unparseable display line (possible partial line being appended)"
                );
                continue;
            }
        };

        if value.get("id").is_none() {
            let legacy = value
                .as_object()
                .is_some_and(|obj| obj.values().any(|field| field.get("msg_id").is_some()));
            if legacy {
                bail!(
                    "legacy display format (old format) not supported: line {} has no top-level id but a variant-level msg_id",
                    line_idx + 1
                );
            }
            // No id at all (e.g. a non-envelope line): not an error, skip.
            continue;
        }

        let Some(id) = value
            .get("id")
            .and_then(serde_json::Value::as_number)
            .and_then(number_to_i32)
        else {
            tracing::warn!(
                line = line_idx + 1,
                "skipping display line with an id that is not a valid i32"
            );
            continue;
        };

        let envelope = match serde_json::from_value::<BroadcastMessage>(value) {
            Ok(envelope) => envelope,
            Err(e) => {
                // Keep the id tracked even when the typed parse fails.
                tracing::warn!(
                    error = %e,
                    line = line_idx + 1,
                    "skipping display line that failed to parse as a BroadcastMessage"
                );
                retained_ids.push(id);
                continue;
            }
        };

        if matches!(envelope.msg, Message::UserMessage { .. }) {
            user_msg_count += 1;
            if envelope.id == target_msg_id {
                target_ordinal = user_msg_count;
                target = Some(envelope.msg);
                cut_line = Some(line_idx);
                break;
            }
        }

        retained_envelopes.push(envelope);
        retained_ids.push(id);
    }

    let (target, cut_line) = match (target, cut_line) {
        (Some(t), Some(l)) => (t, l),
        _ => bail!("target msg_id {} not found", target_msg_id),
    };
    let max_retained_id = retained_ids.iter().copied().max();

    Ok(DisplayCut {
        retained_lines: lines[..cut_line].to_vec(),
        retained_envelopes,
        retained_ids,
        max_retained_id,
        target_ordinal,
        target,
        target_id: target_msg_id,
    })
}

/// Collect the `relative_path` of every `ContentPart::Media` in `User` and
/// `ToolResult` messages, deduped while preserving first-occurrence order.
pub(crate) fn collect_media_refs_from_state(llm_msgs: &[LLMMessage]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut refs = vec![];
    for msg in llm_msgs {
        let parts = match msg {
            LLMMessage::User(parts) => parts,
            LLMMessage::ToolResult { content, .. } => content,
            _ => continue,
        };
        for part in parts {
            if let ContentPart::Media(media) = part {
                let rel = media.relative_path().to_string();
                if seen.insert(rel.clone()) {
                    refs.push(rel);
                }
            }
        }
    }
    refs
}

/// Collect the `media.relative_path()` of every
/// `Message::AssistantMediaOutput` that carries media (`None` media is
/// skipped), deduped while preserving first-occurrence order.
pub(crate) fn collect_media_refs_from_display(envelopes: &[BroadcastMessage]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut refs = vec![];
    for envelope in envelopes {
        if let Message::AssistantMediaOutput {
            media: Some(media), ..
        } = &envelope.msg
        {
            let rel = media.relative_path().to_string();
            if seen.insert(rel.clone()) {
                refs.push(rel);
            }
        }
    }
    refs
}

/// Collect the `conversation_id` of every `SubAgentStart` and
/// `SubAgentContinue` envelope, deduped while preserving first-occurrence
/// order.
pub(crate) fn collect_subagent_ids(envelopes: &[BroadcastMessage]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = vec![];
    for envelope in envelopes {
        let conversation_id = match &envelope.msg {
            Message::SubAgentStart {
                conversation_id, ..
            }
            | Message::SubAgentContinue {
                conversation_id, ..
            } => conversation_id,
            _ => continue,
        };
        if seen.insert(conversation_id.clone()) {
            ids.push(conversation_id.clone());
        }
    }
    ids
}

/// Collect the `tool_call_id` of every `AssistantToolCallStart` and
/// `ToolMessageStart` envelope, deduped while preserving first-occurrence
/// order.
pub(crate) fn collect_tool_call_ids(envelopes: &[BroadcastMessage]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = vec![];
    for envelope in envelopes {
        let tool_call_id = match &envelope.msg {
            Message::AssistantToolCallStart { tool_call_id, .. }
            | Message::ToolMessageStart { tool_call_id, .. } => tool_call_id,
            _ => continue,
        };
        if seen.insert(tool_call_id.clone()) {
            ids.push(tool_call_id.clone());
        }
    }
    ids
}

/// Run the pre-clone validation checks against the full, untruncated
/// conversation state and the display cut. Every check is a hard error that
/// names the file paths and values involved.
pub(crate) fn validate_branch(
    state: &ConversationState,
    cut: &DisplayCut,
    state_path: &Path,
    display_path: &Path,
) -> Result<()> {
    // Check 1: the target user message exists in the state, in bounds.
    let user_count = state
        .llm_msgs
        .iter()
        .filter(|m| matches!(m, LLMMessage::User(_)))
        .count();
    if cut.target_ordinal == 0 || cut.target_ordinal > user_count {
        bail!(
            "branch validation failed ({}): target user message ordinal {} is out of range: conversation state has {} user message(s)",
            state_path.display(),
            cut.target_ordinal,
            user_count
        );
    }

    // Check 2: the target display message matches the ordinal-th state
    // user message (content and media sets, both from the same source data).
    let (target_content, target_media): (&str, &[String]) = match &cut.target {
        Message::UserMessage {
            content,
            media_filenames,
            ..
        } => (content.as_str(), media_filenames),
        _ => bail!(
            "branch validation failed ({}): target display message is not a UserMessage",
            display_path.display()
        ),
    };
    let state_user_index = match nth_user_msg_index(&state.llm_msgs, cut.target_ordinal) {
        Some(i) => i,
        None => bail!(
            "branch validation failed ({}): no user message at ordinal {}",
            state_path.display(),
            cut.target_ordinal
        ),
    };
    let state_user_msg = &state.llm_msgs[state_user_index];
    let LLMMessage::User(parts) = state_user_msg else {
        bail!(
            "branch validation failed ({}): state message at ordinal {} is not a UserMessage",
            state_path.display(),
            cut.target_ordinal
        );
    };
    let state_text = parts.iter().find_map(ContentPart::as_text);
    let state_text = match state_text {
        Some(t) => t,
        None => bail!(
            "branch validation failed ({}): state user message at ordinal {} has no text part (display content was {:?})",
            state_path.display(),
            cut.target_ordinal,
            target_content
        ),
    };
    if target_content != state_text {
        bail!(
            "branch validation failed: display/state user message content mismatch ({} vs {}): display content {:?} != state content {:?}",
            display_path.display(),
            state_path.display(),
            target_content,
            state_text
        );
    }
    let display_media: HashSet<&str> = target_media.iter().map(String::as_str).collect();
    let state_media: HashSet<&str> = parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Media(media) => Some(media.relative_path()),
            ContentPart::Text(_) => None,
        })
        .collect();
    if display_media != state_media {
        bail!(
            "branch validation failed: display/state user message media mismatch ({} vs {}): display media {:?} != state media {:?}",
            display_path.display(),
            state_path.display(),
            display_media,
            state_media
        );
    }

    // Check 3: retained display ids are strictly increasing and every
    // retained id is strictly less than the target id.
    let mut prev: Option<i32> = None;
    for &id in &cut.retained_ids {
        if let Some(p) = prev
            && id <= p
        {
            bail!(
                "branch validation failed ({}): retained display ids are not strictly increasing: {id} follows {p}",
                display_path.display()
            );
        }
        if id >= cut.target_id {
            bail!(
                "branch validation failed ({}): retained display id {id} is not strictly less than target id {}",
                display_path.display(),
                cut.target_id
            );
        }
        prev = Some(id);
    }

    // Check 4: the state's counter is strictly greater than every retained
    // display id.
    if let Some(max_id) = cut.max_retained_id
        && state.msg_id_counter <= max_id
    {
        bail!(
            "branch validation failed ({}): msg_id_counter {} is not strictly greater than max retained display id {}",
            state_path.display(),
            state.msg_id_counter,
            max_id
        );
    }

    Ok(())
}

/// Recursively copy `src` into `dst`, skipping files whose name ends with
/// `.tmp`. Directories are created with 0o700 and regular files copied with
/// 0o600; other file types (symlinks etc.) are skipped with a warning. A
/// missing `src` directory is not an error.
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(dst)
        .with_context(|| format!("failed to create directory {}", dst.display()))?;
    fs::set_permissions(dst, Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", dst.display()))?;
    for entry in
        fs::read_dir(src).with_context(|| format!("failed to read directory {}", src.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", src.display()))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".tmp") {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat {}", src_path.display()))?;
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(&src_path, &dst_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src_path.display(),
                    dst_path.display()
                )
            })?;
            fs::set_permissions(&dst_path, Permissions::from_mode(0o600))
                .with_context(|| format!("failed to set permissions on {}", dst_path.display()))?;
        } else {
            tracing::warn!(
                path = %src_path.display(),
                "skipping non-regular file in recursive copy"
            );
        }
    }
    Ok(())
}

/// Create a staging directory `base/branch-tmp-<pid>-<nonce>` with 0o700
/// permissions, on the same filesystem as the final session location so the
/// eventual rename is atomic.
pub(crate) fn create_staging_dir(base: &Path) -> Result<PathBuf> {
    let nonce: u64 = rand::rng().random();
    let staging = base.join(format!("branch-tmp-{}-{}", std::process::id(), nonce));
    fs::create_dir_all(&staging)
        .with_context(|| format!("failed to create staging dir {}", staging.display()))?;
    fs::set_permissions(&staging, Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "failed to set permissions on staging dir {}",
            staging.display()
        )
    })?;
    Ok(staging)
}

/// Best-effort removal of a staging dir after a failure. `NotFound` is
/// ignored; other cleanup failures are logged.
fn cleanup_staging(staging: &Path) {
    if let Err(e) = fs::remove_dir_all(staging)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            path = %staging.display(),
            error = %e,
            "failed to remove branch staging dir after error"
        );
    }
}

/// Shell-single-quote a value so it can be embedded in a POSIX shell command
/// without interpretation or injection (embedded `'` become `'\''`).
pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Spawn `tmux new-window -c <cwd> "<exe>[-p <profile>] --session=<id> attach"`.
///
/// `cwd` is passed as a plain argv element (tmux treats `-c` as a path, not a
/// shell string); every interpolated value in the inner command is
/// single-quote-escaped so tmux's parser and the pane shell never interpret
/// it. The window closes when the attach command exits.
fn spawn_tmux_attach_tab(
    cwd: &Path,
    exe: &Path,
    profile: Option<&str>,
    session_id: &str,
) -> std::io::Result<std::process::Output> {
    let exe_q = shell_quote(&exe.to_string_lossy());
    let profile_q = profile
        .map(|p| format!(" -p {}", shell_quote(p)))
        .unwrap_or_default();
    let inner_cmd = format!("{exe_q}{profile_q} --session={session_id} attach");
    std::process::Command::new("tmux")
        .arg("new-window")
        .arg("-c")
        .arg(cwd)
        .arg(&inner_cmd)
        .output()
}

/// Copy the media files referenced by the truncated state and the retained
/// display events from the source session's `media/` dir into the staging
/// dir's `media/` dir.
///
/// Every reference is resolved with `fs::canonicalize` and must resolve to a
/// regular file whose parent is the canonical `media/` dir itself; anything
/// else (missing file, empty or directory reference, `..` traversal, symlink
/// pointing outside) is skipped with a warning. Destinations use the
/// canonical file name so no path component is ever derived from untrusted
/// data.
fn copy_media_refs(
    source_dir: &Path,
    staging: &Path,
    truncated_state: &ConversationState,
    cut: &DisplayCut,
) -> Result<()> {
    let media_dir = staging.join("media");
    fs::create_dir_all(&media_dir)
        .with_context(|| format!("failed to create {}", media_dir.display()))?;
    fs::set_permissions(&media_dir, Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", media_dir.display()))?;

    // Union of media refs from the truncated state and the retained display,
    // deduped while preserving first-occurrence order.
    let mut refs = collect_media_refs_from_state(&truncated_state.llm_msgs);
    for rel in collect_media_refs_from_display(&cut.retained_envelopes) {
        if !refs.contains(&rel) {
            refs.push(rel);
        }
    }

    let source_media = source_dir.join("media");
    let source_media_canon = match fs::canonicalize(&source_media) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                path = %source_media.display(),
                "source media dir missing; skipping media copies"
            );
            return Ok(());
        }
        Err(e) => {
            return Err(e).with_context(|| {
                format!("failed to resolve media dir {}", source_media.display())
            });
        }
    };
    for rel in &refs {
        let src = source_media.join(rel);
        let src_canon = match fs::canonicalize(&src) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(path = %src.display(), "source media file missing; skipping");
                continue;
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to resolve media {}", src.display()));
            }
        };
        if src_canon.parent() != Some(source_media_canon.as_path()) || !src_canon.is_file() {
            tracing::warn!(
                path = %src.display(),
                "skipping media reference that is not a file inside the media dir"
            );
            continue;
        }
        let Some(name) = src_canon.file_name() else {
            tracing::warn!(path = %src.display(), "skipping media reference with no file name");
            continue;
        };
        let dst = media_dir.join(name);
        fs::copy(&src_canon, &dst).with_context(|| {
            format!(
                "failed to copy media {} to {}",
                src_canon.display(),
                dst.display()
            )
        })?;
        fs::set_permissions(&dst, Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set permissions on {}", dst.display()))?;
    }
    Ok(())
}

/// Build the branch session's content inside the staging dir.
///
/// Subagent dirs and tool-call files are copied from canonicalized source
/// paths verified to be direct children of the source session dir, and are
/// written to the staging dir under their canonical file name — ids from the
/// display file are never used as path components, so a crafted id cannot
/// escape either directory.
pub(crate) fn build_branch_content(
    source_dir: &Path,
    staging: &Path,
    truncated_state: &ConversationState,
    cut: &DisplayCut,
) -> Result<()> {
    // conversation-state.json, written atomically (tmp + rename).
    let state_json = serde_json::to_string_pretty(truncated_state)?;
    let state_tmp = staging.join("conversation-state.json.tmp");
    let state_target = staging.join("conversation-state.json");
    fs::write(&state_tmp, &state_json)
        .with_context(|| format!("failed to write {}", state_tmp.display()))?;
    fs::rename(&state_tmp, &state_target)
        .with_context(|| format!("failed to rename {}", state_target.display()))?;

    // session-meta.json: source mode, description from the truncated summary.
    let mode = tcode_runtime::session::read_session_mode(source_dir)?;
    tcode_runtime::session::update_session_meta_from_summary(
        staging,
        &truncated_state.summary(),
        mode,
    )?;

    // display.jsonl: the retained prefix. Always created, even when empty.
    let mut display_content = String::new();
    for line in &cut.retained_lines {
        display_content.push_str(line);
        display_content.push('\n');
    }
    let display_target = staging.join("display.jsonl");
    fs::write(&display_target, display_content)
        .with_context(|| format!("failed to write {}", display_target.display()))?;

    // media/.
    copy_media_refs(source_dir, staging, truncated_state, cut)?;

    let source_dir_canon = fs::canonicalize(source_dir).with_context(|| {
        format!(
            "failed to resolve source session dir {}",
            source_dir.display()
        )
    })?;

    // subagent-<conversation_id>/ dirs.
    for id in collect_subagent_ids(&cut.retained_envelopes) {
        let src = source_dir.join(format!("subagent-{}", id));
        let src_canon = match fs::canonicalize(&src) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(path = %src.display(), "source subagent dir missing; skipping copy");
                continue;
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to resolve {}", src.display()));
            }
        };
        if src_canon.parent() != Some(source_dir_canon.as_path()) || !src_canon.is_dir() {
            tracing::warn!(
                path = %src.display(),
                "skipping subagent path that is not a directory inside the source session dir"
            );
            continue;
        }
        let Some(name) = src_canon.file_name() else {
            tracing::warn!(path = %src.display(), "skipping subagent path with no file name");
            continue;
        };
        copy_dir_recursive(&src_canon, &staging.join(name))?;
    }

    // tool-call-<id>.jsonl and tool-call-<id>-status.txt.
    for id in collect_tool_call_ids(&cut.retained_envelopes) {
        for suffix in [".jsonl", "-status.txt"] {
            let src = source_dir.join(format!("tool-call-{}{}", id, suffix));
            let src_canon = match fs::canonicalize(&src) {
                Ok(c) => c,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    tracing::warn!(path = %src.display(), "source tool-call file missing; skipping");
                    continue;
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("failed to resolve {}", src.display()));
                }
            };
            if src_canon.parent() != Some(source_dir_canon.as_path()) || !src_canon.is_file() {
                tracing::warn!(
                    path = %src.display(),
                    "skipping tool-call path that is not a file inside the source session dir"
                );
                continue;
            }
            let Some(name) = src_canon.file_name() else {
                tracing::warn!(path = %src.display(), "skipping tool-call path with no file name");
                continue;
            };
            let dst = staging.join(name);
            fs::copy(&src_canon, &dst).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    src_canon.display(),
                    dst.display()
                )
            })?;
        }
    }

    Ok(())
}

/// Commit the staging dir with an atomic rename. If the target session id
/// already exists (race with a concurrent creator — including an empty
/// directory, which Linux `rename` would otherwise silently replace), a
/// fresh id is generated and the rename retried, up to 3 total attempts.
pub(crate) fn commit_branch_staging(
    base: &Path,
    staging: &Path,
    initial_id: String,
) -> Result<String> {
    let mut new_id = initial_id;
    for _ in 0..3 {
        let target = base.join(&new_id);
        if target.exists() {
            tracing::warn!(
                target = %target.display(),
                "branch commit raced with a concurrent session creator; retrying with a fresh id"
            );
            new_id = tcode_runtime::session::generate_unique_session_id(base, None)?;
            continue;
        }
        match fs::rename(staging, &target) {
            Ok(()) => return Ok(new_id),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                tracing::warn!(
                    target = %target.display(),
                    "branch commit raced with a concurrent session creator; retrying with a fresh id"
                );
                new_id = tcode_runtime::session::generate_unique_session_id(base, None)?;
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "failed to rename staging dir {} to {}",
                        staging.display(),
                        target.display()
                    )
                });
            }
        }
    }
    bail!(
        "failed to commit branch session: the session id kept colliding with an existing session after 3 attempts"
    )
}

/// Clone the source session up to (not including) the target user message
/// into a new independent session and open it in a new tmux tab.
pub(crate) fn run_branch(
    profile: Option<&str>,
    source_session_id: &str,
    target_msg_id: i32,
) -> Result<()> {
    let base = tcode_runtime::session::base_path()?;
    let source_dir = base.join(source_session_id);
    let display_path = source_dir.join("display.jsonl");
    let state_path = source_dir.join("conversation-state.json");

    // Step 1: read and cut the source display at the target line.
    let content = fs::read_to_string(&display_path).with_context(|| {
        format!(
            "failed to read display file {}: target msg_id {} cannot be found",
            display_path.display(),
            target_msg_id
        )
    })?;
    let lines: Vec<String> = content.lines().map(str::to_string).collect();
    let cut = truncate_display_at_msg_id(&lines, target_msg_id)
        .with_context(|| format!("in display file {}", display_path.display()))?;

    // Step 2: read the full state, validate against it, then truncate it.
    let state_json = fs::read_to_string(&state_path)
        .with_context(|| format!("failed to read conversation state {}", state_path.display()))?;
    let state: ConversationState = serde_json::from_str(&state_json).with_context(|| {
        format!(
            "failed to parse conversation state {}",
            state_path.display()
        )
    })?;
    validate_branch(&state, &cut, &state_path, &display_path)?;
    let truncated_state = truncate_state_at_user(state, cut.target_ordinal)?;

    // Step 3: reserve a fresh session id.
    let new_id = tcode_runtime::session::generate_unique_session_id(&base, None)?;

    // Step 4: staging dir on the same filesystem as the final location.
    let staging = create_staging_dir(&base)?;

    // Steps 5-7: build content and commit. Any failure removes the staging
    // dir so nothing half-written remains in the sessions dir.
    if let Err(e) = build_branch_content(&source_dir, &staging, &truncated_state, &cut) {
        cleanup_staging(&staging);
        return Err(e);
    }
    let new_id = match commit_branch_staging(&base, &staging, new_id) {
        Ok(id) => id,
        Err(e) => {
            cleanup_staging(&staging);
            return Err(e);
        }
    };

    // Step 8: open the branch in a new tmux tab. The session is already
    // committed and complete, so a spawn failure is a partial success: keep
    // the session, report the tab error, and let the user attach manually.
    let spawn_outcome = (|| -> std::io::Result<std::process::Output> {
        let cwd = std::env::current_dir()?;
        let exe = std::env::current_exe()?;
        spawn_tmux_attach_tab(&cwd, &exe, profile, &new_id)
    })();
    match spawn_outcome {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            println!(
                "Branched to session {}; failed to open the tab: {} - run \"tcode attach --session={}\" later",
                new_id, stderr, new_id
            );
        }
        Err(e) => {
            println!(
                "Branched to session {}; failed to open the tab: {} - run \"tcode attach --session={}\" later",
                new_id, e, new_id
            );
        }
    }

    // Step 9: report success.
    println!("Branched to session {}", new_id);
    Ok(())
}
