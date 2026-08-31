//! Tests for the global content-addressed Lua module cache: `cache_path()`,
//! `ensure_module_cache`, and `ensure_skills_module_cache`.

use crate::test_support::{HomeGuard, TestDir};
use llm_rs::skill::{SkillMeta, SkillSource};
use std::os::unix::fs::PermissionsExt;

/// `cache_path()` resolves to `$HOME/.tcode/cache` (via `dirs::home_dir()`)
/// and creates the directory with 0700 permissions.
#[test]
fn cache_path_returns_home_cache_and_creates_dir() -> anyhow::Result<()> {
    let dir = TestDir::new("module_cache");
    let _guard = HomeGuard::set(dir.path());

    let expected = dir.path().join(".tcode").join("cache");
    let actual = crate::session::cache_path()?;

    assert_eq!(actual, expected);
    assert!(expected.is_dir(), "cache dir should exist");
    let mode = std::fs::metadata(&expected)?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "cache dir must be 0700");
    Ok(())
}

/// `ensure_module_cache` on a fresh base writes the plain `TCODE_LUA` and both
/// tree-sitter query files into `<content_hash>/`.
#[test]
fn ensure_module_cache_creates_entry_on_fresh_base() -> anyhow::Result<()> {
    let base = TestDir::new("module_cache");
    let hash = super::content_hash();

    let entry = super::ensure_module_cache(&base, &hash)?;

    assert_eq!(entry, base.join(&hash));
    assert_eq!(
        std::fs::read_to_string(entry.join("tcode.lua"))?,
        super::TCODE_LUA
    );
    assert_eq!(
        std::fs::read_to_string(entry.join("queries/tcode/injections.scm"))?,
        super::INJECTIONS_SCM
    );
    assert_eq!(
        std::fs::read_to_string(entry.join("queries/tcode/highlights.scm"))?,
        super::HIGHLIGHTS_SCM
    );
    Ok(())
}

/// `ensure_module_cache` is idempotent: an existing entry dir is left
/// untouched, even if its `tcode.lua` differs from the embedded source.
#[test]
fn ensure_module_cache_skips_existing_entry() -> anyhow::Result<()> {
    let base = TestDir::new("module_cache");
    let hash = super::content_hash();
    let entry_dir = base.join(&hash);
    std::fs::create_dir_all(&entry_dir)?;
    let sentinel = "sentinel: existing entry must not be overwritten";
    std::fs::write(entry_dir.join("tcode.lua"), sentinel)?;

    let entry = super::ensure_module_cache(&base, &hash)?;

    assert_eq!(entry, entry_dir);
    assert_eq!(std::fs::read_to_string(entry.join("tcode.lua"))?, sentinel);
    Ok(())
}

/// `ensure_skills_module_cache` with dummy skills creates a dir distinct from
/// the plain `content_hash` dir whose `tcode.lua` carries both the skills
/// preamble marker and the embedded source, and a second call with the same
/// skills does not rewrite it.
#[test]
fn ensure_skills_module_cache_writes_entry_and_skips_existing() -> anyhow::Result<()> {
    let base = TestDir::new("module_cache");
    // Dummy skill whose SKILL.md does not exist: `load_skill_body` fails, so
    // the preamble contains empty tables but still carries its markers.
    let skill_dir = base.path().join("skills").join("test-skill");
    let skills = vec![SkillMeta {
        name: "test-skill".to_string(),
        description: Some("a test skill".to_string()),
        when_to_use: None,
        dir: skill_dir.clone(),
        skill_file: skill_dir.join("SKILL.md"),
        source: SkillSource::UserTcode,
        user_invocable: true,
        disable_model_invocation: false,
    }];

    let module = super::build_skills_module(&skills);
    let full_hash = super::skills_hash(&module);
    assert_ne!(
        full_hash,
        super::content_hash(),
        "skills entry dir must differ from the plain content_hash dir"
    );

    let entry = super::ensure_skills_module_cache(&base, &full_hash, &module)?;
    assert_eq!(entry, base.join(&full_hash));
    let content = std::fs::read_to_string(entry.join("tcode.lua"))?;
    assert!(
        content.contains("_G.tcode_skills"),
        "skills preamble marker missing from module"
    );
    assert!(
        content.contains("function M.setup_tool_call_display"),
        "embedded TCODE_LUA marker missing from module"
    );
    assert_eq!(
        content, module,
        "skills entry tcode.lua must equal the module content exactly"
    );

    // A second call with the same skills must not rewrite the existing entry.
    let sentinel = "sentinel: second call must not rewrite";
    std::fs::write(entry.join("tcode.lua"), sentinel)?;
    let entry_again = super::ensure_skills_module_cache(&base, &full_hash, &module)?;
    assert_eq!(entry_again, entry);
    assert_eq!(std::fs::read_to_string(entry.join("tcode.lua"))?, sentinel);
    Ok(())
}

/// `ensure_module_cache` removes a stale temp dir left by a killed attempt at
/// the exact temp path: its contents must not leak into the published entry,
/// and the cache base must end up with only the entry dir (no `.tmp.*`
/// leftovers).
#[test]
fn ensure_module_cache_cleans_stale_temp_dir() -> anyhow::Result<()> {
    let base = TestDir::new("module_cache");
    let hash = super::content_hash();
    let stale = base.join(format!(".{hash}.tmp.{}", std::process::id()));
    std::fs::create_dir_all(&stale)?;
    std::fs::write(stale.join("junk"), "stale partial entry")?;

    let entry = super::ensure_module_cache(&base, &hash)?;

    assert_eq!(entry, base.join(&hash));
    assert_eq!(
        std::fs::read_to_string(entry.join("tcode.lua"))?,
        super::TCODE_LUA
    );
    assert!(
        !entry.join("junk").exists(),
        "stale temp dir contents must not leak into the published entry"
    );
    let names: Vec<std::ffi::OsString> = std::fs::read_dir(base.path())?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(
        names,
        vec![std::ffi::OsString::from(hash.as_str())],
        "cache base should contain only the published entry dir"
    );
    Ok(())
}

/// A publish that races another process publishing the same entry first (the
/// build closure pre-creates the target dir, so the rename fails with
/// ENOTEMPTY) must return the existing entry and discard its own temp dir.
#[test]
fn publish_cache_entry_race_loses_to_existing_entry() -> anyhow::Result<()> {
    let base = TestDir::new("module_cache");
    let name = "race-entry";
    let entry_dir = base.join(name);
    let tmp_dir = base.join(format!(".{name}.tmp.{}", std::process::id()));

    // The build closure simulates the racing process by publishing the target
    // entry before this publish's rename runs.
    let entry = super::publish_cache_entry(&base, name, |tmp| {
        std::fs::create_dir_all(&entry_dir)?;
        std::fs::write(entry_dir.join("tcode.lua"), "winner")?;
        std::fs::create_dir_all(tmp)?;
        std::fs::write(tmp.join("tcode.lua"), "loser")?;
        Ok(())
    })?;

    assert_eq!(entry, entry_dir);
    assert_eq!(
        std::fs::read_to_string(entry_dir.join("tcode.lua"))?,
        "winner",
        "the pre-existing entry must win the race"
    );
    assert!(!tmp_dir.exists(), "loser temp dir must be removed");
    Ok(())
}

/// A build failure must remove the partially-built temp dir before the error
/// is returned.
#[test]
fn publish_cache_entry_cleans_up_on_build_failure() -> anyhow::Result<()> {
    let base = TestDir::new("module_cache");
    let name = "fail-entry";
    let tmp_dir = base.join(format!(".{name}.tmp.{}", std::process::id()));

    let result = super::publish_cache_entry(&base, name, |tmp| {
        std::fs::create_dir_all(tmp)?;
        std::fs::write(tmp.join("tcode.lua"), "partial")?;
        Err(anyhow::anyhow!("simulated build failure"))
    });

    assert!(result.is_err(), "build failure must propagate");
    assert!(
        !tmp_dir.exists(),
        "temp dir must be removed on build failure"
    );
    Ok(())
}
