use std::collections::HashMap;
use std::fs::{self, Permissions};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use jieba_rs::Jieba;
use llm_rs::conversation::ConversationState;
use llm_rs::llm::LLMMessage;
use llm_rs::media::ContentPart;
use parking_lot::Mutex;
use rusqlite::ffi::ErrorCode;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::session;

const DB_NAME: &str = "fts.db";
const SCHEMA_VERSION: i64 = 1;
const TOMBSTONE_MSG_COUNT: i64 = -1;
const TOMBSTONE_GRACE_NS: i64 = 5 * 60 * 1_000_000_000;
const HIGHLIGHT_START: &str = "\x01";
const HIGHLIGHT_END: &str = "\x02";
const SEARCH_BUSY_TIMEOUT_MS: u64 = 25;

static JIEBA: LazyLock<Jieba> = LazyLock::new(Jieba::new);
static RECOVERY_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub session_id: String,
    pub snippet: String,
}

#[derive(Debug)]
pub enum IndexProgress {
    Progress { current: usize, total: usize },
    Done(Result<()>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FtsMeta {
    msg_count: i64,
    mtime_ns: i64,
    file_size: i64,
}

#[derive(Debug)]
struct IndexableMessage {
    msg_index: usize,
    role: &'static str,
    content: String,
}

pub fn index_session(base: &Path, session_id: &str) -> Result<()> {
    session::validate_session_id(session_id)?;

    let state_path = base.join(session_id).join("conversation-state.json");
    let metadata = match fs::metadata(&state_path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tombstone_missing_session(base, session_id)?;
            return Ok(());
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to stat {}", state_path.display()));
        }
    };

    let file_mtime_ns = metadata_mtime_ns(&metadata);
    let file_size =
        i64::try_from(metadata.len()).context("conversation-state.json is too large")?;

    let mut conn = open_connection(base)?;
    let meta = lookup_meta(&conn, session_id)?;
    index_existing_session_with_connection(
        &mut conn,
        session_id,
        &state_path,
        file_mtime_ns,
        file_size,
        meta,
    )?;
    Ok(())
}

pub fn ensure_indexed(base: &Path) -> Result<()> {
    ensure_indexed_inner(base, None)
}

pub fn ensure_indexed_with_progress(
    base: &Path,
    progress_tx: std::sync::mpsc::Sender<IndexProgress>,
) {
    let result = ensure_indexed_inner(base, Some(&progress_tx));
    if let Err(_disconnected) = progress_tx.send(IndexProgress::Done(result)) {}
}

fn ensure_indexed_inner(
    base: &Path,
    progress_tx: Option<&std::sync::mpsc::Sender<IndexProgress>>,
) -> Result<()> {
    let sessions = session::list_sessions_at(base)
        .with_context(|| format!("failed to list sessions under {}", base.display()))?;
    let total = sessions.len();
    let mut conn = open_connection(base)?;

    conn.execute_batch("CREATE TEMP TABLE current_sessions(session_id TEXT PRIMARY KEY);")?;
    {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for session_id in &sessions {
            tx.execute(
                "INSERT INTO current_sessions(session_id) VALUES (?1)",
                params![session_id],
            )?;
        }
        tx.execute(
            "DELETE FROM fts_meta WHERE msg_count = -1 AND mtime_ns < ?1",
            params![wall_clock_ns().saturating_sub(TOMBSTONE_GRACE_NS)],
        )?;
        tx.commit()?;
    }

    let metas = load_all_meta(&conn)?;
    let mut changed = false;
    for (index, session_id) in sessions.iter().enumerate() {
        let state_path = base.join(session_id).join("conversation-state.json");
        match fs::metadata(&state_path) {
            Ok(metadata) => {
                let file_mtime_ns = metadata_mtime_ns(&metadata);
                let file_size = i64::try_from(metadata.len())
                    .context("conversation-state.json is too large")?;
                changed |= index_existing_session_with_connection(
                    &mut conn,
                    session_id,
                    &state_path,
                    file_mtime_ns,
                    file_size,
                    metas.get(session_id).copied(),
                )?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tombstone_missing_session_with_connection(&mut conn, session_id)?;
                changed = true;
            }
            Err(e) => {
                return Err(e).with_context(|| format!("failed to stat {}", state_path.display()));
            }
        }

        if let Some(progress_tx) = progress_tx
            && progress_tx
                .send(IndexProgress::Progress {
                    current: index + 1,
                    total,
                })
                .is_err()
        {
            return Ok(());
        }
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let deleted_fts = tx.execute(
        "DELETE FROM session_fts
         WHERE session_id NOT IN (SELECT session_id FROM current_sessions)",
        [],
    )?;
    let deleted_meta = tx.execute(
        "DELETE FROM fts_meta
         WHERE session_id NOT IN (SELECT session_id FROM current_sessions)
           AND msg_count != -1",
        [],
    )?;
    if changed || deleted_fts > 0 || deleted_meta > 0 {
        tx.execute(
            "INSERT INTO session_fts(session_fts) VALUES('optimize')",
            [],
        )?;
    }
    tx.commit()?;

    Ok(())
}

pub fn remove_session(base: &Path, session_id: &str) -> Result<()> {
    session::validate_session_id(session_id)?;
    let mut conn = open_connection(base)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR REPLACE INTO fts_meta
             (session_id, msg_count, mtime_ns, file_size, schema_version)
         VALUES (?1, -1, ?2, 0, ?3)",
        params![session_id, wall_clock_ns(), SCHEMA_VERSION],
    )?;
    tx.execute(
        "DELETE FROM session_fts WHERE session_id = ?1",
        params![session_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn search(base: &Path, query: &str) -> Result<Vec<SearchResult>> {
    let cancel = AtomicBool::new(false);
    search_with_cancel(base, query, &cancel)
}

pub fn search_with_cancel(
    base: &Path,
    query: &str,
    cancel: &AtomicBool,
) -> Result<Vec<SearchResult>> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(Vec::new());
    }

    let Some(match_query) = build_match_query(query) else {
        return session::list_sessions_at(base)
            .with_context(|| format!("failed to list sessions under {}", base.display()))
            .map(|sessions| {
                sessions
                    .into_iter()
                    .map(|session_id| SearchResult {
                        session_id,
                        snippet: String::new(),
                    })
                    .collect()
            });
    };

    let Some(conn) = open_search_connection(base)? else {
        return Ok(Vec::new());
    };
    let cancel_addr = cancel as *const AtomicBool as usize;
    conn.progress_handler(
        1_000,
        Some(move || unsafe { (*(cancel_addr as *const AtomicBool)).load(Ordering::Relaxed) }),
    );

    let mut results = Vec::new();
    let mut stmt = match conn.prepare(
        "SELECT session_id, snippet_text
         FROM (
             SELECT session_id,
                    snippet_text,
                    score,
                    ROW_NUMBER() OVER (
                        PARTITION BY session_id
                        ORDER BY score ASC, CAST(msg_index AS INTEGER) DESC
                    ) AS rn
             FROM (
                 SELECT session_id,
                        msg_index,
                        snippet(session_fts, 3, ?2, ?3, '...', 64) AS snippet_text,
                        bm25(session_fts) AS score
                 FROM session_fts
                 WHERE session_fts MATCH ?1
             )
         )
         WHERE rn = 1
         ORDER BY score ASC
         LIMIT 100",
    ) {
        Ok(stmt) => stmt,
        Err(e) if cancel.load(Ordering::Relaxed) || is_transient_sqlite_lock(&e) => {
            return Ok(results);
        }
        Err(e) => return Err(e.into()),
    };
    let mut rows = match stmt.query(params![match_query, HIGHLIGHT_START, HIGHLIGHT_END]) {
        Ok(rows) => rows,
        Err(e) if cancel.load(Ordering::Relaxed) || is_transient_sqlite_lock(&e) => {
            return Ok(results);
        }
        Err(e) => return Err(e.into()),
    };

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(results);
        }
        match rows.next() {
            Ok(Some(row)) => {
                results.push(SearchResult {
                    session_id: row.get(0)?,
                    snippet: row.get(1)?,
                });
            }
            Ok(None) => return Ok(results),
            Err(e) if cancel.load(Ordering::Relaxed) || is_transient_sqlite_lock(&e) => {
                return Ok(results);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn open_connection(base: &Path) -> Result<Connection> {
    fs::create_dir_all(base).with_context(|| format!("failed to create {}", base.display()))?;
    fs::set_permissions(base, Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set permissions on {}", base.display()))?;

    let db_path = db_path(base);
    let conn = open_configured(&db_path)?;
    if validate_or_initialize_connection(&conn).is_ok() {
        return Ok(conn);
    }
    drop(conn);

    let _guard = RECOVERY_LOCK.lock();
    let conn = open_configured(&db_path)?;
    if validate_or_initialize_connection(&conn).is_ok() {
        return Ok(conn);
    }
    drop(conn);

    remove_db_files(&db_path)?;
    let conn = open_configured(&db_path)?;
    create_schema(&conn)?;
    fts_integrity_check(&conn)?;
    Ok(conn)
}

fn open_configured(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .with_context(|| format!("failed to open {}", db_path.display()))?;
    fs::set_permissions(db_path, Permissions::from_mode(0o600))
        .with_context(|| format!("failed to set permissions on {}", db_path.display()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")?;
    Ok(conn)
}

fn open_search_connection(base: &Path) -> Result<Option<Connection>> {
    let db_path = db_path(base);
    if !db_path.is_file() {
        return Ok(None);
    }

    let conn = match Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(e) if is_transient_sqlite_lock(&e) => return Ok(None),
        Err(e) if is_sqlite_error_code(&e, ErrorCode::CannotOpen) && !db_path.exists() => {
            return Ok(None);
        }
        Err(e) => return Err(e).with_context(|| format!("failed to open {}", db_path.display())),
    };

    conn.busy_timeout(std::time::Duration::from_millis(SEARCH_BUSY_TIMEOUT_MS))?;
    if let Err(e) = conn.execute_batch("PRAGMA query_only=ON;") {
        if is_transient_sqlite_lock(&e) {
            return Ok(None);
        }
        return Err(e.into());
    }

    match search_schema_exists(&conn) {
        Ok(true) => Ok(Some(conn)),
        Ok(false) => Ok(None),
        Err(e) if is_transient_sqlite_lock(&e) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn search_schema_exists(conn: &Connection) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_fts'",
        [],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
}

fn is_transient_sqlite_lock(e: &rusqlite::Error) -> bool {
    is_sqlite_error_code(e, ErrorCode::DatabaseBusy)
        || is_sqlite_error_code(e, ErrorCode::DatabaseLocked)
}

fn is_sqlite_error_code(e: &rusqlite::Error, code: ErrorCode) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(error, _) if error.code == code)
}

fn validate_or_initialize_connection(conn: &Connection) -> Result<()> {
    quick_check(conn)?;
    create_schema(conn)?;
    fts_integrity_check(conn)
}

fn quick_check(conn: &Connection) -> Result<()> {
    let result: String = conn.pragma_query_value(None, "quick_check", |row| row.get(0))?;
    if result == "ok" {
        Ok(())
    } else {
        bail!("SQLite quick_check failed: {result}")
    }
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS fts_meta (
             session_id     TEXT PRIMARY KEY,
             msg_count      INTEGER NOT NULL,
             mtime_ns       INTEGER NOT NULL,
             file_size      INTEGER NOT NULL DEFAULT 0,
             schema_version INTEGER NOT NULL DEFAULT 1
         );

         CREATE VIRTUAL TABLE IF NOT EXISTS session_fts USING fts5(
             session_id UNINDEXED,
             msg_index UNINDEXED,
             role UNINDEXED,
             content,
             tokenize='unicode61'
         );",
    )?;
    Ok(())
}

fn fts_integrity_check(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO session_fts(session_fts) VALUES('integrity-check')",
        [],
    )?;
    Ok(())
}

fn remove_db_files(db_path: &Path) -> Result<()> {
    for path in [
        db_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", db_path.display())),
        PathBuf::from(format!("{}-shm", db_path.display())),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("failed to remove {}", path.display()));
            }
        }
    }
    Ok(())
}

fn lookup_meta(conn: &Connection, session_id: &str) -> Result<Option<FtsMeta>> {
    conn.query_row(
        "SELECT msg_count, mtime_ns, file_size FROM fts_meta WHERE session_id = ?1",
        params![session_id],
        |row| {
            Ok(FtsMeta {
                msg_count: row.get(0)?,
                mtime_ns: row.get(1)?,
                file_size: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn load_all_meta(conn: &Connection) -> Result<HashMap<String, FtsMeta>> {
    let mut stmt =
        conn.prepare("SELECT session_id, msg_count, mtime_ns, file_size FROM fts_meta")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            FtsMeta {
                msg_count: row.get(1)?,
                mtime_ns: row.get(2)?,
                file_size: row.get(3)?,
            },
        ))
    })?;

    let mut metas = HashMap::new();
    for row in rows {
        let (session_id, meta) = row?;
        metas.insert(session_id, meta);
    }
    Ok(metas)
}

fn index_existing_session_with_connection(
    conn: &mut Connection,
    session_id: &str,
    state_path: &Path,
    file_mtime_ns: i64,
    file_size: i64,
    meta: Option<FtsMeta>,
) -> Result<bool> {
    let mut meta = meta;
    if should_skip_existing_session(meta, file_mtime_ns, file_size) {
        return Ok(false);
    }

    let current_meta = lookup_meta(conn, session_id)?;
    if current_meta != meta {
        if should_skip_existing_session(current_meta, file_mtime_ns, file_size) {
            return Ok(false);
        }
        meta = current_meta;
    }

    let json = match fs::read_to_string(state_path) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tombstone_missing_session_with_connection(conn, session_id)?;
            return Ok(true);
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", state_path.display()));
        }
    };

    let state = match serde_json::from_str::<ConversationState>(&json) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                path = %state_path.display(),
                error = %e,
                "failed to parse conversation state for FTS indexing; keeping existing index"
            );
            return Ok(false);
        }
    };

    let total_msg_count = i64::try_from(state.llm_msgs.len()).context("too many messages")?;
    let start_index = match meta {
        None => 0,
        Some(meta) if meta.msg_count < total_msg_count && meta.msg_count >= 0 => {
            usize::try_from(meta.msg_count).context("invalid indexed message count")?
        }
        Some(_) => 0,
    };
    let full_reindex = matches!(meta, Some(meta) if meta.msg_count >= total_msg_count);
    let messages = collect_indexable_messages(&state.llm_msgs, start_index);

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if full_reindex {
        tx.execute(
            "DELETE FROM session_fts
             WHERE session_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM fts_meta WHERE session_id = ?1 AND msg_count = -1
               )",
            params![session_id],
        )?;
    }

    insert_messages(&tx, session_id, &messages)?;

    match meta {
        None => {
            tx.execute(
                "INSERT INTO fts_meta (session_id, msg_count, mtime_ns, file_size, schema_version)
                 SELECT ?1, ?2, ?3, ?4, ?5
                 WHERE NOT EXISTS (
                     SELECT 1 FROM fts_meta WHERE session_id = ?1 AND msg_count = -1
                 )",
                params![
                    session_id,
                    total_msg_count,
                    file_mtime_ns,
                    file_size,
                    SCHEMA_VERSION
                ],
            )?;
        }
        Some(_) => {
            tx.execute(
                "UPDATE fts_meta
                 SET msg_count = ?2, mtime_ns = ?3, file_size = ?4, schema_version = ?5
                 WHERE session_id = ?1 AND msg_count != -1",
                params![
                    session_id,
                    total_msg_count,
                    file_mtime_ns,
                    file_size,
                    SCHEMA_VERSION
                ],
            )?;
        }
    }

    tx.commit()?;
    Ok(true)
}

fn should_skip_existing_session(meta: Option<FtsMeta>, file_mtime_ns: i64, file_size: i64) -> bool {
    let Some(meta) = meta else {
        return false;
    };
    meta.msg_count == TOMBSTONE_MSG_COUNT
        || (meta.mtime_ns == file_mtime_ns && meta.file_size == file_size)
}

fn tombstone_missing_session(base: &Path, session_id: &str) -> Result<()> {
    let mut conn = open_connection(base)?;
    tombstone_missing_session_with_connection(&mut conn, session_id)
}

fn tombstone_missing_session_with_connection(
    conn: &mut Connection,
    session_id: &str,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR REPLACE INTO fts_meta
             (session_id, msg_count, mtime_ns, file_size, schema_version)
         VALUES (?1, -1, ?2, 0, ?3)",
        params![session_id, wall_clock_ns(), SCHEMA_VERSION],
    )?;
    tx.execute(
        "DELETE FROM session_fts WHERE session_id = ?1",
        params![session_id],
    )?;
    tx.commit()?;
    Ok(())
}

fn insert_messages(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
    messages: &[IndexableMessage],
) -> Result<()> {
    for message in messages {
        tx.execute(
            "INSERT INTO session_fts (session_id, msg_index, role, content)
             SELECT ?1, ?2, ?3, ?4
             WHERE NOT EXISTS (
                 SELECT 1 FROM fts_meta WHERE session_id = ?1 AND msg_count = -1
             )",
            params![
                session_id,
                i64::try_from(message.msg_index).context("message index is too large")?,
                message.role,
                segment_text(&message.content),
            ],
        )?;
    }
    Ok(())
}

fn collect_indexable_messages(
    messages: &[LLMMessage],
    start_index: usize,
) -> Vec<IndexableMessage> {
    messages
        .iter()
        .enumerate()
        .skip(start_index)
        .filter_map(|(msg_index, message)| match message {
            LLMMessage::User(parts) => {
                let content = parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text(text) => Some(text.as_str()),
                        ContentPart::Media(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                non_empty_indexable(msg_index, "User", content)
            }
            LLMMessage::Assistant { content, .. } => {
                non_empty_indexable(msg_index, "Assistant", content.clone())
            }
            LLMMessage::System(_) | LLMMessage::ToolResult { .. } => None,
        })
        .collect()
}

fn non_empty_indexable(
    msg_index: usize,
    role: &'static str,
    content: String,
) -> Option<IndexableMessage> {
    if content.trim().is_empty() {
        None
    } else {
        Some(IndexableMessage {
            msg_index,
            role,
            content,
        })
    }
}

fn build_match_query(query: &str) -> Option<String> {
    let raw_tokens: Vec<&str> = if contains_cjk(query) {
        JIEBA.cut(query, false)
    } else {
        query.split_whitespace().collect()
    };
    let tokens = raw_tokens
        .into_iter()
        .map(str::trim)
        .filter(|token| token_has_search_char(token))
        .collect::<Vec<_>>();

    if tokens.is_empty() {
        return None;
    }

    Some(
        tokens
            .iter()
            .enumerate()
            .map(|(index, token)| {
                let quoted = format!("\"{}\"", token.replace('"', "\"\""));
                if index + 1 == tokens.len() {
                    format!("{quoted}*")
                } else {
                    quoted
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn segment_text(text: &str) -> String {
    if contains_cjk(text) {
        JIEBA.cut(text, false).join(" ")
    } else {
        text.to_string()
    }
}

fn contains_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

fn token_has_search_char(token: &str) -> bool {
    token.chars().any(|ch| ch.is_alphanumeric() || is_cjk(ch))
}

fn is_cjk(ch: char) -> bool {
    ('\u{3400}'..='\u{4DBF}').contains(&ch)
        || ('\u{4E00}'..='\u{9FFF}').contains(&ch)
        || ('\u{F900}'..='\u{FAFF}').contains(&ch)
        || ('\u{20000}'..='\u{2A6DF}').contains(&ch)
        || ('\u{2A700}'..='\u{2B73F}').contains(&ch)
        || ('\u{2B740}'..='\u{2B81F}').contains(&ch)
        || ('\u{2B820}'..='\u{2CEAF}').contains(&ch)
}

fn metadata_mtime_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(metadata.mtime_nsec())
}

fn wall_clock_ns() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

fn db_path(base: &Path) -> PathBuf {
    base.join(DB_NAME)
}
