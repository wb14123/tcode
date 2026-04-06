use llm_rs::permission::{SCOPE_BASH, ScopedPermissionManager};

use super::command_permission::{command_matches_permission, has_command_permission};

#[test]
fn command_matches_exact() {
    assert!(command_matches_permission("git", "git"));
}

#[test]
fn command_matches_with_args() {
    assert!(command_matches_permission("git", "git diff"));
    assert!(command_matches_permission("git", "git add ."));
    assert!(command_matches_permission("git", "git push origin"));
}

#[test]
fn command_does_not_match_prefix_without_boundary() {
    assert!(!command_matches_permission("git", "gitabc"));
}

#[test]
fn subcommand_match() {
    assert!(command_matches_permission("git push", "git push"));
    assert!(command_matches_permission(
        "git push",
        "git push origin main"
    ));
}

#[test]
fn subcommand_does_not_match_different_subcommand() {
    assert!(!command_matches_permission("git push", "git add"));
}

#[test]
fn cargo_matches() {
    assert!(command_matches_permission("cargo", "cargo build"));
    assert!(command_matches_permission("cargo", "cargo test --release"));
    assert!(!command_matches_permission("cargo", "cargoabc"));
}

#[test]
fn hierarchical_lookup_most_specific_first() {
    // Create a permission manager with "git add" stored
    let pm = std::sync::Arc::new(llm_rs::permission::PermissionManager::new(
        std::env::temp_dir().join(format!(
            "llm-rs-test-cmd-perm-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        )),
    ));

    let key = llm_rs::permission::PermissionKey {
        tool: SCOPE_BASH.to_string(),
        key: "command".to_string(),
        value: "git add".to_string(),
    };
    pm.resolve(
        &key,
        &llm_rs::permission::PermissionDecision::AllowSession,
        None,
    )
    .expect("resolve should succeed");

    let scoped = ScopedPermissionManager::new(
        "bash",
        pm,
        std::sync::Arc::new(|| {}),
        std::sync::Arc::new(|| {}),
        None,
    );

    // "git add src/main.rs" should match via prefix "git add"
    let tokens: Vec<String> = vec![
        "git".to_string(),
        "add".to_string(),
        "src/main.rs".to_string(),
    ];
    assert!(has_command_permission(&scoped, &tokens));

    // "git push" should NOT match "git add"
    let tokens2: Vec<String> = vec!["git".to_string(), "push".to_string()];
    assert!(!has_command_permission(&scoped, &tokens2));
}

#[test]
fn hierarchical_lookup_base_command() {
    let pm = std::sync::Arc::new(llm_rs::permission::PermissionManager::new(
        std::env::temp_dir().join(format!(
            "llm-rs-test-cmd-perm-base-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        )),
    ));

    let key = llm_rs::permission::PermissionKey {
        tool: SCOPE_BASH.to_string(),
        key: "command".to_string(),
        value: "cargo".to_string(),
    };
    pm.resolve(
        &key,
        &llm_rs::permission::PermissionDecision::AllowSession,
        None,
    )
    .expect("resolve should succeed");

    let scoped = ScopedPermissionManager::new(
        "bash",
        pm,
        std::sync::Arc::new(|| {}),
        std::sync::Arc::new(|| {}),
        None,
    );

    // "cargo build" should match via base "cargo"
    let tokens: Vec<String> = vec!["cargo".to_string(), "build".to_string()];
    assert!(has_command_permission(&scoped, &tokens));

    // "cargo test --release" should also match
    let tokens2: Vec<String> = vec![
        "cargo".to_string(),
        "test".to_string(),
        "--release".to_string(),
    ];
    assert!(has_command_permission(&scoped, &tokens2));
}

#[test]
fn permission_npm_match() {
    assert!(command_matches_permission("npm", "npm install"));
    assert!(command_matches_permission("npm", "npm run build"));
    assert!(!command_matches_permission("npm", "npx create"));
}
