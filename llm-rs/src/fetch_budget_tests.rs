use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::conversation::ConversationState;
use crate::fetch_budget::{DEFAULT_WEB_FETCH_CHAR_BUDGET, WebFetchBudget, WebFetchBudgetFile};
use crate::llm::ChatOptions;

/// Per-test temp dir under the workspace target dir; removed on drop
/// (cleanup runs on success and on panic).
struct TestDir(PathBuf);

impl TestDir {
    fn new(module: &str) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../target/test-tmp/{module}"));
        std::fs::create_dir_all(&root).expect("failed to create test root");
        let dir = root.join(uuid::Uuid::new_v4().to_string());
        // Cleanup before: remove any stale leftover at this exact path.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("failed to create test dir");
        Self(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Path of the budget file a node in `dir` persists to.
fn budget_path(dir: &Path) -> PathBuf {
    dir.join("web-fetch-budget.json")
}

fn node(remaining: u32, id: &str) -> Arc<WebFetchBudget> {
    WebFetchBudget::new(remaining, id.to_string(), None, None)
}

// ─── new ────────────────────────────────────────────────────────────────────

#[test]
fn new_without_parent_keeps_remaining_and_id() {
    let root = node(1000, "root");

    assert_eq!(root.get(), 1000);
    assert_eq!(root.id(), "root");
}

#[test]
fn new_with_parent_clamps_to_parent() {
    let parent = node(500, "parent");

    let below = WebFetchBudget::new(100, "below".to_string(), Some(&parent), None);
    let equal = WebFetchBudget::new(500, "equal".to_string(), Some(&parent), None);
    let above = WebFetchBudget::new(10_000, "above".to_string(), Some(&parent), None);

    assert_eq!(below.get(), 100);
    assert_eq!(equal.get(), 500);
    assert_eq!(above.get(), 500);
    // Clamping the child never touches the parent.
    assert_eq!(parent.get(), 500);
}

#[test]
fn new_saturates_at_u32_max() {
    let root = node(u32::MAX, "root");
    assert_eq!(root.get(), u32::MAX);

    let child = WebFetchBudget::new(u32::MAX, "child".to_string(), Some(&root), None);
    assert_eq!(child.get(), u32::MAX);

    assert_eq!(root.charge(u32::MAX), u32::MAX);
    assert_eq!(root.get(), 0);
    assert_eq!(child.get(), 0);
}

#[test]
fn default_budget_constant_is_400k() {
    assert_eq!(DEFAULT_WEB_FETCH_CHAR_BUDGET, 400_000);
}

// ─── new vs restore ─────────────────────────────────────────────────────────

#[test]
fn new_writes_file_with_parent_and_remaining() -> anyhow::Result<()> {
    let dir = TestDir::new("fetch-budget/new-writes-file");
    let root_file = WebFetchBudgetFile::read(dir.path());
    assert!(root_file.is_none(), "no file before the node exists");

    let root = WebFetchBudget::new(
        1000,
        "root".to_string(),
        None,
        Some(budget_path(dir.path())),
    );
    let written = WebFetchBudgetFile::read(dir.path()).expect("new wrote the file");
    assert_eq!(written.parent, None);
    assert_eq!(written.remaining, 1000);

    let child_dir = dir.path().join("child");
    let _child = WebFetchBudget::new(
        500,
        "child".to_string(),
        Some(&root),
        Some(budget_path(&child_dir)),
    );
    let written = WebFetchBudgetFile::read(&child_dir).expect("child file written");
    assert_eq!(written.parent.as_deref(), Some("root"));
    assert_eq!(written.remaining, 500);
    Ok(())
}

#[test]
fn restore_never_writes() -> anyhow::Result<()> {
    let dir = TestDir::new("fetch-budget/restore-never-writes");
    let path = budget_path(dir.path());
    let sentinel = "{\n  \"parent\": null,\n  \"remaining\": 42\n}\n";
    std::fs::write(&path, sentinel)?;
    let before = std::fs::metadata(&path)?.modified()?;

    let restored = WebFetchBudget::restore(10, "id".to_string(), None, None, Some(path.clone()));

    assert_eq!(restored.get(), 10, "restore keeps the value it was given");
    assert_eq!(
        std::fs::metadata(&path)?.modified()?,
        before,
        "restore must not touch the file"
    );
    assert_eq!(std::fs::read_to_string(&path)?, sentinel);
    Ok(())
}

#[test]
fn restore_keeps_parent_id_when_parent_could_not_be_loaded() -> anyhow::Result<()> {
    let dir = TestDir::new("fetch-budget/restore-missing-parent");
    let orphan_dir = dir.path().join("orphan");
    std::fs::create_dir_all(&orphan_dir)?;
    std::fs::write(
        budget_path(&orphan_dir),
        "{\"parent\": \"missing-parent\", \"remaining\": 5}",
    )?;

    let file = WebFetchBudgetFile::read(&orphan_dir).expect("file parses");
    assert_eq!(file.parent.as_deref(), Some("missing-parent"));

    let restored = WebFetchBudget::restore(
        file.remaining,
        "orphan".to_string(),
        None,
        file.parent.clone(),
        Some(budget_path(&orphan_dir)),
    );
    assert_eq!(restored.get(), 5);

    let after = WebFetchBudgetFile::read(&orphan_dir).expect("file still parses");
    assert_eq!(
        after.parent.as_deref(),
        Some("missing-parent"),
        "loading without the ancestor must not erase the link"
    );
    assert_eq!(after.remaining, 5);
    Ok(())
}

#[test]
fn restore_clamps_to_the_live_parent() {
    let parent = node(120, "parent");

    let restored = WebFetchBudget::restore(
        500,
        "child".to_string(),
        Some(Arc::clone(&parent)),
        Some("parent".to_string()),
        None,
    );

    assert_eq!(restored.get(), 120);
}

// ─── get ────────────────────────────────────────────────────────────────────

#[test]
fn get_is_minimum_over_the_chain() {
    let root = node(1000, "root");
    let mid = WebFetchBudget::new(700, "mid".to_string(), Some(&root), None);
    let leaf = WebFetchBudget::new(300, "leaf".to_string(), Some(&mid), None);

    assert_eq!(root.get(), 1000);
    assert_eq!(mid.get(), 700);
    assert_eq!(leaf.get(), 300);

    assert_eq!(root.charge(900), 900);
    assert_eq!(root.get(), 100);
    assert_eq!(mid.get(), 100);
    assert_eq!(leaf.get(), 100);
}

// ─── charge ─────────────────────────────────────────────────────────────────

#[test]
fn charge_partial_returns_granted_amount() {
    let root = node(100, "root");

    assert_eq!(root.charge(30), 30);
    assert_eq!(root.get(), 70);
}

#[test]
fn charge_exact_empties_the_pool() {
    let root = node(100, "root");

    assert_eq!(root.charge(100), 100);
    assert_eq!(root.get(), 0);
}

#[test]
fn charge_more_than_remaining_grants_only_what_is_left() {
    let root = node(100, "root");

    assert_eq!(root.charge(150), 100);
    assert_eq!(root.get(), 0);
}

#[test]
fn charge_on_exhausted_budget_returns_zero() {
    let root = node(0, "root");

    assert_eq!(root.charge(10), 0);
    assert_eq!(root.get(), 0);
}

#[test]
fn charge_deducts_the_grant_from_every_node_in_the_chain() {
    let root = node(100, "root");
    let child = WebFetchBudget::new(60, "child".to_string(), Some(&root), None);

    let granted = child.charge(10);

    assert_eq!(granted, 10);
    assert_eq!(root.get(), 90);
    assert_eq!(child.get(), 50);
}

#[test]
fn siblings_consume_one_pool() {
    let parent = node(100, "parent");
    let a = WebFetchBudget::new(100, "a".to_string(), Some(&parent), None);
    let b = WebFetchBudget::new(100, "b".to_string(), Some(&parent), None);

    let granted_a = a.charge(60);
    let granted_b = b.charge(60);

    assert_eq!(granted_a, 60);
    assert_eq!(granted_b, 40, "only the pool's leftover is granted");
    assert_eq!(granted_a + granted_b, 100);
    assert_eq!(parent.get(), 0);
    assert_eq!(a.get(), 0);
    assert_eq!(b.get(), 0);
}

#[test]
fn three_level_chain_deducts_from_every_ancestor() {
    let root = node(300, "root");
    let mid = WebFetchBudget::new(200, "mid".to_string(), Some(&root), None);
    let leaf = WebFetchBudget::new(100, "leaf".to_string(), Some(&mid), None);

    assert_eq!(leaf.charge(150), 100, "the leaf's own allowance caps it");
    assert_eq!(root.get(), 200);
    assert_eq!(mid.get(), 100);
    assert_eq!(leaf.get(), 0);
    assert_eq!(leaf.charge(10), 0);

    assert_eq!(mid.charge(50), 50);
    assert_eq!(root.get(), 150);
    assert_eq!(mid.get(), 50);
}

#[test]
fn child_cannot_spend_past_an_ancestor() {
    let root = node(100, "root");
    let child = WebFetchBudget::new(100, "child".to_string(), Some(&root), None);

    assert_eq!(root.charge(90), 90);
    assert_eq!(child.get(), 10);
    assert_eq!(child.charge(50), 10, "capped by the ancestor's leftover");
    assert_eq!(child.get(), 0);
    assert_eq!(root.get(), 0);
}

#[test]
fn concurrent_charges_never_exceed_the_pool() {
    let root = node(250, "root");
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let root = Arc::clone(&root);
            std::thread::spawn(move || root.charge(100))
        })
        .collect();

    let granted: u32 = threads
        .into_iter()
        .map(|handle| handle.join().expect("worker thread joined"))
        .sum();

    assert_eq!(granted, 250);
    assert_eq!(root.get(), 0);
}

#[test]
fn concurrent_sibling_charges_never_exceed_the_shared_pool() {
    let parent = node(250, "parent");
    let left = WebFetchBudget::new(250, "left".to_string(), Some(&parent), None);
    let right = WebFetchBudget::new(250, "right".to_string(), Some(&parent), None);

    let threads: Vec<_> = [Arc::clone(&left), Arc::clone(&right)]
        .into_iter()
        .flat_map(|child| (0..4).map(move |_| Arc::clone(&child)))
        .map(|child| std::thread::spawn(move || child.charge(100)))
        .collect();

    let granted: u32 = threads
        .into_iter()
        .map(|handle| handle.join().expect("worker thread joined"))
        .sum();

    assert_eq!(granted, 250, "the shared pool bounds sibling grants");
    assert_eq!(parent.get(), 0);
    assert_eq!(left.get(), 0);
    assert_eq!(right.get(), 0);
}

// ─── file persistence ───────────────────────────────────────────────────────

#[test]
fn charge_updates_the_node_and_every_ancestor_file() -> anyhow::Result<()> {
    let dir = TestDir::new("fetch-budget/charge-updates-files");
    let child_dir = dir.path().join("child");
    let root = WebFetchBudget::new(
        1000,
        "root".to_string(),
        None,
        Some(budget_path(dir.path())),
    );
    let child = WebFetchBudget::new(
        600,
        "child".to_string(),
        Some(&root),
        Some(budget_path(&child_dir)),
    );

    assert_eq!(child.charge(100), 100);

    let root_file = WebFetchBudgetFile::read(dir.path()).expect("root file");
    assert_eq!(root_file.remaining, 900);
    assert_eq!(root_file.parent, None);

    let child_file = WebFetchBudgetFile::read(&child_dir).expect("child file");
    assert_eq!(child_file.remaining, 500);
    assert_eq!(child_file.parent.as_deref(), Some("root"));
    Ok(())
}

#[test]
fn persisted_values_survive_a_reload() -> anyhow::Result<()> {
    let dir = TestDir::new("fetch-budget/reload");
    let child_dir = dir.path().join("child");
    let root = WebFetchBudget::new(800, "root".to_string(), None, Some(budget_path(dir.path())));
    let child = WebFetchBudget::new(
        500,
        "child".to_string(),
        Some(&root),
        Some(budget_path(&child_dir)),
    );

    child.charge(120);

    let root_file = WebFetchBudgetFile::read(dir.path()).expect("root file");
    let child_file = WebFetchBudgetFile::read(&child_dir).expect("child file");
    assert_eq!(root_file.remaining, 680);
    assert_eq!(child_file.remaining, 380);

    // Rebuild the tree from disk, the way a resume does.
    let reloaded_root = WebFetchBudget::restore(
        root_file.remaining,
        "root".to_string(),
        None,
        root_file.parent.clone(),
        Some(budget_path(dir.path())),
    );
    let reloaded_child = WebFetchBudget::restore(
        child_file.remaining,
        "child".to_string(),
        Some(Arc::clone(&reloaded_root)),
        child_file.parent.clone(),
        Some(budget_path(&child_dir)),
    );

    assert_eq!(reloaded_root.get(), 680);
    assert_eq!(reloaded_child.get(), 380);
    Ok(())
}

#[test]
fn charge_zero_keeps_the_files_intact() -> anyhow::Result<()> {
    let dir = TestDir::new("fetch-budget/charge-zero");
    let path = budget_path(dir.path());
    let root = WebFetchBudget::new(400, "root".to_string(), None, Some(path.clone()));
    std::fs::remove_file(&path)?;

    assert_eq!(root.charge(0), 0);

    assert!(!path.exists(), "a zero grant must not rewrite the file");
    assert_eq!(root.get(), 400);
    Ok(())
}

#[test]
fn missing_file_returns_none() {
    let dir = TestDir::new("fetch-budget/missing-file");

    assert!(WebFetchBudgetFile::read(dir.path()).is_none());
}

#[test]
fn invalid_file_returns_none_without_panicking() {
    let dir = TestDir::new("fetch-budget/invalid-file");
    let path = budget_path(dir.path());

    std::fs::write(&path, "not json at all").expect("write invalid file");
    assert!(WebFetchBudgetFile::read(dir.path()).is_none());

    std::fs::write(&path, "{\"remaining\": }").expect("write truncated json");
    assert!(WebFetchBudgetFile::read(dir.path()).is_none());

    // An old file without the `parent` key still parses.
    std::fs::write(&path, "{\"remaining\": 7}").expect("write legacy file");
    let file = WebFetchBudgetFile::read(dir.path()).expect("legacy file parses");
    assert_eq!(file.parent, None);
    assert_eq!(file.remaining, 7);
}

// ─── ConversationState serde ────────────────────────────────────────────────

fn conversation_state(web_fetch_budget_id: Option<&str>) -> ConversationState {
    ConversationState {
        id: "conv-1".to_string(),
        model: "test-model".to_string(),
        llm_msgs: Vec::new(),
        chat_options: ChatOptions::default(),
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_creation_tokens: 0,
        total_cache_read_tokens: 0,
        aggregate_input_tokens: 0,
        aggregate_output_tokens: 0,
        aggregate_cache_creation_tokens: 0,
        aggregate_cache_read_tokens: 0,
        single_turn: false,
        subagent_depth: 0,
        web_fetch_budget_id: web_fetch_budget_id.map(str::to_string),
    }
}

#[test]
fn conversation_state_roundtrips_the_budget_id() -> anyhow::Result<()> {
    for id in [Some("conv-1"), None] {
        let state = conversation_state(id);

        let json = serde_json::to_string(&state)?;
        let back: ConversationState = serde_json::from_str(&json)?;

        assert_eq!(back.web_fetch_budget_id.as_deref(), id);
    }
    Ok(())
}

#[test]
fn conversation_state_without_the_budget_id_deserializes_to_none() -> anyhow::Result<()> {
    let mut json = serde_json::to_value(conversation_state(None))?;
    json.as_object_mut()
        .expect("a state serializes to a JSON object")
        .remove("web_fetch_budget_id");

    let back: ConversationState = serde_json::from_value(json)?;

    assert!(back.web_fetch_budget_id.is_none());
    assert_eq!(back.id, "conv-1");
    Ok(())
}
