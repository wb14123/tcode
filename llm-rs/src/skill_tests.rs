use std::path::PathBuf;

use crate::skill::{
    SkillMeta, SkillSource, format_skill_entry, list_skill_files, load_skill_content,
    scan_skills_from_dirs,
};

/// A temporary directory under `target/test-tmp/` that is removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> anyhow::Result<Self> {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../target/test-tmp")
            .join(name);
        if base.exists() {
            std::fs::remove_dir_all(&base)?;
        }
        std::fs::create_dir_all(&base)?;
        Ok(Self(base))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_skill(
    name: &str,
    desc: Option<&str>,
    when: Option<&str>,
    dir: PathBuf,
    skill_file: PathBuf,
) -> SkillMeta {
    SkillMeta {
        name: name.to_string(),
        description: desc.map(String::from),
        when_to_use: when.map(String::from),
        dir,
        skill_file,
        source: SkillSource::ProjectTcode,
    }
}

/// A fake path for tests that don't touch the filesystem.
fn fake_path() -> PathBuf {
    PathBuf::from("/fake/skill/dir")
}

// ─── load_skill_content tests ───────────────────────────────────────────────

#[test]
fn test_load_skill_content_with_frontmatter() -> anyhow::Result<()> {
    let tmp = TempDir::new("load_frontmatter")?;
    let skill_file = tmp.path().join("SKILL.md");
    let content = "---\nname: my-skill\ndescription: A cool skill\n---\n\nBody content here.\n";
    std::fs::write(&skill_file, content)?;

    let meta = test_skill(
        "my-skill",
        Some("A cool skill"),
        None,
        tmp.path().to_path_buf(),
        skill_file,
    );
    let loaded = load_skill_content(&meta)?;
    assert_eq!(loaded, content);
    Ok(())
}

#[test]
fn test_load_skill_content_substitutes_skill_dir() -> anyhow::Result<()> {
    let tmp = TempDir::new("load_subst")?;
    let skill_file = tmp.path().join("SKILL.md");
    let content = "Look in ${CLAUDE_SKILL_DIR}/templates for examples.\n";
    std::fs::write(&skill_file, content)?;

    let meta = test_skill("sub-test", None, None, tmp.path().to_path_buf(), skill_file);
    let loaded = load_skill_content(&meta)?;
    let expected = format!("Look in {}/templates for examples.\n", tmp.path().display());
    assert_eq!(loaded, expected);
    Ok(())
}

#[test]
fn test_load_skill_content_preserves_other_vars() -> anyhow::Result<()> {
    let tmp = TempDir::new("load_preserve")?;
    let skill_file = tmp.path().join("SKILL.md");
    let content = "Use $ARGUMENTS and ${CLAUDE_SESSION_ID} as-is.\n";
    std::fs::write(&skill_file, content)?;

    let meta = test_skill(
        "preserve-test",
        None,
        None,
        tmp.path().to_path_buf(),
        skill_file,
    );
    let loaded = load_skill_content(&meta)?;
    assert_eq!(loaded, content);
    Ok(())
}

// ─── list_skill_files tests ─────────────────────────────────────────────────

#[test]
fn test_list_skill_files_excludes_skill_md() -> anyhow::Result<()> {
    let tmp = TempDir::new("list_excludes")?;
    std::fs::write(tmp.path().join("SKILL.md"), "skill")?;
    std::fs::write(tmp.path().join("helper.sh"), "#!/bin/bash")?;
    std::fs::write(tmp.path().join("template.txt"), "template")?;

    let meta = test_skill(
        "test",
        None,
        None,
        tmp.path().to_path_buf(),
        tmp.path().join("SKILL.md"),
    );
    let files = list_skill_files(&meta);

    let names: Vec<&str> = files
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    assert!(!names.contains(&"SKILL.md"));
    assert!(names.contains(&"helper.sh"));
    assert!(names.contains(&"template.txt"));
    assert_eq!(files.len(), 2);
    Ok(())
}

#[test]
fn test_list_skill_files_caps_at_10() -> anyhow::Result<()> {
    let tmp = TempDir::new("list_cap10")?;
    std::fs::write(tmp.path().join("SKILL.md"), "skill")?;
    for i in 0..15 {
        std::fs::write(tmp.path().join(format!("file_{i:02}.txt")), "content")?;
    }

    let meta = test_skill(
        "test",
        None,
        None,
        tmp.path().to_path_buf(),
        tmp.path().join("SKILL.md"),
    );
    let files = list_skill_files(&meta);
    assert_eq!(files.len(), 10);
    Ok(())
}

#[test]
fn test_list_skill_files_excludes_subdirectories() -> anyhow::Result<()> {
    let tmp = TempDir::new("list_no_subdirs")?;
    std::fs::write(tmp.path().join("SKILL.md"), "skill")?;
    std::fs::write(tmp.path().join("file.txt"), "content")?;
    std::fs::create_dir(tmp.path().join("subdir"))?;
    std::fs::write(tmp.path().join("subdir").join("nested.txt"), "nested")?;

    let meta = test_skill(
        "test",
        None,
        None,
        tmp.path().to_path_buf(),
        tmp.path().join("SKILL.md"),
    );
    let files = list_skill_files(&meta);

    let names: Vec<&str> = files
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    assert_eq!(files.len(), 1);
    assert!(names.contains(&"file.txt"));
    assert!(!names.contains(&"subdir"));
    Ok(())
}

#[test]
fn test_list_skill_files_sorted_alphabetically() -> anyhow::Result<()> {
    let tmp = TempDir::new("list_sorted")?;
    std::fs::write(tmp.path().join("SKILL.md"), "skill")?;
    std::fs::write(tmp.path().join("zebra.txt"), "")?;
    std::fs::write(tmp.path().join("apple.txt"), "")?;
    std::fs::write(tmp.path().join("mango.txt"), "")?;

    let meta = test_skill(
        "test",
        None,
        None,
        tmp.path().to_path_buf(),
        tmp.path().join("SKILL.md"),
    );
    let files = list_skill_files(&meta);

    let names: Vec<&str> = files
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    assert_eq!(names, vec!["apple.txt", "mango.txt", "zebra.txt"]);
    Ok(())
}

// ─── format_skill_entry tests ───────────────────────────────────────────────

#[test]
fn test_format_skill_entry_truncation() -> anyhow::Result<()> {
    let long_desc = "A".repeat(300);
    let skill = test_skill(
        "long-skill",
        Some(&long_desc),
        None,
        fake_path(),
        fake_path().join("SKILL.md"),
    );
    let entry = format_skill_entry(&skill);
    // "- long-skill: " is 14 chars + 300 chars desc = 314 > 250
    assert!(entry.len() <= 254); // 250 + up to 4 bytes for the … char
    assert!(entry.ends_with('\u{2026}')); // …
    Ok(())
}

#[test]
fn test_format_skill_entry_no_truncation() -> anyhow::Result<()> {
    let skill = test_skill(
        "short",
        Some("Brief"),
        None,
        fake_path(),
        fake_path().join("SKILL.md"),
    );
    let entry = format_skill_entry(&skill);
    assert_eq!(entry, "- short: Brief");
    assert!(!entry.ends_with('\u{2026}'));
    Ok(())
}

// ─── load_skill_content edge cases ──────────────────────────────────────────

#[test]
fn test_load_skill_content_no_frontmatter() -> anyhow::Result<()> {
    let tmp = TempDir::new("load_no_fm")?;
    let skill_file = tmp.path().join("SKILL.md");
    let content = "Just plain markdown body, no frontmatter.\n\n## Section\nMore content.\n";
    std::fs::write(&skill_file, content)?;

    let meta = test_skill("plain", None, None, tmp.path().to_path_buf(), skill_file);
    let loaded = load_skill_content(&meta)?;
    assert_eq!(loaded, content);
    Ok(())
}

#[test]
fn test_load_skill_content_empty_file() -> anyhow::Result<()> {
    let tmp = TempDir::new("load_empty")?;
    let skill_file = tmp.path().join("SKILL.md");
    std::fs::write(&skill_file, "")?;

    let meta = test_skill("empty", None, None, tmp.path().to_path_buf(), skill_file);
    let loaded = load_skill_content(&meta)?;
    assert_eq!(loaded, "");
    Ok(())
}

#[test]
fn test_load_skill_content_crlf_line_endings() -> anyhow::Result<()> {
    let tmp = TempDir::new("load_crlf")?;
    let skill_file = tmp.path().join("SKILL.md");
    let content = "---\r\nname: test\r\n---\r\nUse ${CLAUDE_SKILL_DIR}/foo\r\n";
    std::fs::write(&skill_file, content)?;

    let meta = test_skill("test", None, None, tmp.path().to_path_buf(), skill_file);
    let loaded = load_skill_content(&meta)?;
    let expected = content.replace("${CLAUDE_SKILL_DIR}", &tmp.path().to_string_lossy());
    assert_eq!(loaded, expected);
    Ok(())
}

#[test]
fn test_load_skill_content_unclosed_frontmatter() -> anyhow::Result<()> {
    let tmp = TempDir::new("load_unclosed")?;
    let skill_file = tmp.path().join("SKILL.md");
    let content = "---\nname: test\nbody content without closing delimiter\n";
    std::fs::write(&skill_file, content)?;

    let meta = test_skill("unclosed", None, None, tmp.path().to_path_buf(), skill_file);
    let loaded = load_skill_content(&meta)?;
    // load_skill_content returns the raw content (no frontmatter stripping)
    assert_eq!(loaded, content);
    Ok(())
}

// ─── format_skill_entry edge cases ──────────────────────────────────────────

#[test]
fn test_format_skill_entry_with_all_fields() -> anyhow::Result<()> {
    let skill = test_skill(
        "commit",
        Some("Generate commit messages"),
        Some("When the user asks to commit"),
        fake_path(),
        fake_path().join("SKILL.md"),
    );
    let entry = format_skill_entry(&skill);
    assert_eq!(
        entry,
        "- commit: Generate commit messages - When the user asks to commit"
    );
    Ok(())
}

#[test]
fn test_format_skill_entry_name_only() -> anyhow::Result<()> {
    let skill = test_skill(
        "minimal",
        None,
        None,
        fake_path(),
        fake_path().join("SKILL.md"),
    );
    let entry = format_skill_entry(&skill);
    assert_eq!(entry, "- minimal");
    Ok(())
}

#[test]
fn test_format_skill_entry_when_to_use_only() -> anyhow::Result<()> {
    let skill = test_skill(
        "deploy",
        None,
        Some("When deploying to production"),
        fake_path(),
        fake_path().join("SKILL.md"),
    );
    let entry = format_skill_entry(&skill);
    assert_eq!(entry, "- deploy - When deploying to production");
    Ok(())
}

// ─── scan_skills_from_dirs tests ────────────────────────────────────────────

#[test]
fn test_scan_dedup_first_wins() -> anyhow::Result<()> {
    let dir1 = TempDir::new("scan_dedup_1")?;
    let dir2 = TempDir::new("scan_dedup_2")?;

    // Create skill "commit" in both dirs
    let skill1 = dir1.path().join("commit");
    std::fs::create_dir(&skill1)?;
    std::fs::write(
        skill1.join("SKILL.md"),
        "---\nname: commit\ndescription: First definition\n---\nBody 1\n",
    )?;

    let skill2 = dir2.path().join("commit");
    std::fs::create_dir(&skill2)?;
    std::fs::write(
        skill2.join("SKILL.md"),
        "---\nname: commit\ndescription: Second definition\n---\nBody 2\n",
    )?;

    let dirs = vec![
        (dir1.path().to_path_buf(), SkillSource::ProjectTcode),
        (dir2.path().to_path_buf(), SkillSource::UserTcode),
    ];

    let (skills, warnings) = scan_skills_from_dirs(&dirs);

    // Only the first one should be kept
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "commit");
    assert_eq!(skills[0].description.as_deref(), Some("First definition"));

    // Should have a warning about the shadow
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("skipped: already defined"));
    assert!(warnings[0].contains("commit"));
    Ok(())
}

#[test]
fn test_scan_nonexistent_dir_skipped() -> anyhow::Result<()> {
    let real_dir = TempDir::new("scan_nonexist")?;
    let skill = real_dir.path().join("my-skill");
    std::fs::create_dir(&skill)?;
    std::fs::write(skill.join("SKILL.md"), "---\nname: my-skill\n---\nBody\n")?;

    let dirs = vec![
        (
            PathBuf::from("/tmp/this-dir-definitely-does-not-exist-xyz123"),
            SkillSource::ProjectTcode,
        ),
        (real_dir.path().to_path_buf(), SkillSource::UserTcode),
    ];

    let (skills, warnings) = scan_skills_from_dirs(&dirs);

    // Non-existent dir silently skipped, real dir found
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "my-skill");
    // No warnings about the missing dir
    assert!(warnings.is_empty());
    Ok(())
}

#[test]
fn test_scan_skill_name_from_dir_when_no_frontmatter_name() -> anyhow::Result<()> {
    let base = TempDir::new("scan_dir_name")?;
    let skill = base.path().join("my-cool-skill");
    std::fs::create_dir(&skill)?;
    // No `name` field in frontmatter
    std::fs::write(
        skill.join("SKILL.md"),
        "---\ndescription: A cool skill\n---\nBody content\n",
    )?;

    let dirs = vec![(base.path().to_path_buf(), SkillSource::ProjectTcode)];

    let (skills, warnings) = scan_skills_from_dirs(&dirs);

    assert!(warnings.is_empty());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "my-cool-skill");
    assert_eq!(skills[0].description.as_deref(), Some("A cool skill"));
    Ok(())
}
