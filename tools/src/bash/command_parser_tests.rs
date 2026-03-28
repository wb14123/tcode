use std::path::PathBuf;

use super::command_parser::{
    CommandClassification, extract_paths_from_args, looks_like_path, parse_command,
    try_decompose_complex,
};

#[test]
fn classify_read_command() {
    let parsed = parse_command("cat /etc/hosts");
    assert!(matches!(
        parsed.classification,
        CommandClassification::ReadCommand { .. }
    ));
    if let CommandClassification::ReadCommand { paths } = parsed.classification {
        assert_eq!(paths, vec![PathBuf::from("/etc/hosts")]);
    }
}

#[test]
fn classify_write_command() {
    let parsed = parse_command("mkdir /tmp/test-dir");
    assert!(matches!(
        parsed.classification,
        CommandClassification::WriteCommand { .. }
    ));
    if let CommandClassification::WriteCommand { paths } = parsed.classification {
        assert_eq!(paths, vec![PathBuf::from("/tmp/test-dir")]);
    }
}

#[test]
fn classify_other_simple() {
    let parsed = parse_command("git add src/main.rs");
    assert!(matches!(
        parsed.classification,
        CommandClassification::OtherSimple { .. }
    ));
    if let CommandClassification::OtherSimple { tokens } = parsed.classification {
        assert_eq!(tokens, vec!["git", "add", "src/main.rs"]);
    }
}

#[test]
fn classify_pipeline_as_complex() {
    let parsed = parse_command("cat file.txt | grep foo");
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn classify_and_list_as_complex() {
    let parsed = parse_command("cd /tmp && ls");
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn classify_or_list_as_complex() {
    let parsed = parse_command("test -f foo || echo missing");
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn classify_semicolon_as_complex() {
    let parsed = parse_command("echo a; echo b");
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn classify_command_substitution_as_complex() {
    let parsed = parse_command("echo $(whoami)");
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn classify_subshell_as_complex() {
    let parsed = parse_command("(echo hello)");
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn extract_output_redirect() {
    let parsed = parse_command("echo hello > /tmp/out.txt");
    assert!(!parsed.redirections.output_files.is_empty());
    assert_eq!(
        parsed.redirections.output_files[0],
        PathBuf::from("/tmp/out.txt")
    );
}

#[test]
fn fd_redirect_2_to_1_is_not_a_file() {
    let parsed = parse_command("cargo build 2>&1");
    assert!(parsed.redirections.output_files.is_empty());
    assert!(parsed.redirections.input_files.is_empty());
}

#[test]
fn fd_redirect_stderr_to_stdout_is_not_a_file() {
    let parsed = parse_command("ls >&2");
    assert!(parsed.redirections.output_files.is_empty());
    assert!(parsed.redirections.input_files.is_empty());
}

#[test]
fn extract_input_redirect() {
    let parsed = parse_command("wc -l < /tmp/input.txt");
    assert!(!parsed.redirections.input_files.is_empty());
    assert_eq!(
        parsed.redirections.input_files[0],
        PathBuf::from("/tmp/input.txt")
    );
}

#[test]
fn extract_append_redirect() {
    let parsed = parse_command("echo hello >> /tmp/log.txt");
    assert!(!parsed.redirections.output_files.is_empty());
    assert_eq!(
        parsed.redirections.output_files[0],
        PathBuf::from("/tmp/log.txt")
    );
}

#[test]
fn skip_flags_in_path_extraction() {
    let paths =
        extract_paths_from_args(&["-n".to_string(), "10".to_string(), "/etc/hosts".to_string()]);
    assert_eq!(paths, vec![PathBuf::from("/etc/hosts")]);
}

#[test]
fn looks_like_path_cases() {
    assert!(looks_like_path("/absolute/path"));
    assert!(looks_like_path("./relative"));
    assert!(looks_like_path("../parent"));
    assert!(looks_like_path("dir/file"));
    assert!(!looks_like_path("justword"));
}

#[test]
fn simple_ls_command() {
    let parsed = parse_command("ls");
    assert!(matches!(
        parsed.classification,
        CommandClassification::OtherSimple { .. }
    ));
    if let CommandClassification::OtherSimple { tokens } = parsed.classification {
        assert_eq!(tokens, vec!["ls"]);
    }
}

#[test]
fn read_command_with_flags() {
    let parsed = parse_command("head -n 20 /var/log/syslog");
    assert!(matches!(
        parsed.classification,
        CommandClassification::ReadCommand { .. }
    ));
    if let CommandClassification::ReadCommand { paths } = parsed.classification {
        assert_eq!(paths, vec![PathBuf::from("/var/log/syslog")]);
    }
}

#[test]
fn cat_with_redirect_has_output_file() {
    let parsed = parse_command("cat /etc/hosts > /tmp/hosts-copy");
    // cat is a read command but has output redirect
    assert!(matches!(
        parsed.classification,
        CommandClassification::ReadCommand { .. }
    ));
    if let CommandClassification::ReadCommand { paths } = &parsed.classification {
        assert_eq!(paths, &[PathBuf::from("/etc/hosts")]);
    }
    assert_eq!(
        parsed.redirections.output_files,
        vec![PathBuf::from("/tmp/hosts-copy")]
    );
}

#[test]
fn variable_expansion_in_command_is_complex() {
    let parsed = parse_command("$CMD arg1 arg2");
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn touch_is_write_command() {
    let parsed = parse_command("touch /tmp/newfile.txt");
    assert!(matches!(
        parsed.classification,
        CommandClassification::WriteCommand { .. }
    ));
}

#[test]
fn rm_is_not_whitelisted() {
    let parsed = parse_command("rm /tmp/somefile");
    assert!(matches!(
        parsed.classification,
        CommandClassification::OtherSimple { .. }
    ));
}

#[test]
fn parser_diff_is_read() {
    let parsed = parse_command("diff /tmp/a.txt /tmp/b.txt");
    assert!(matches!(
        parsed.classification,
        CommandClassification::ReadCommand { .. }
    ));
    if let CommandClassification::ReadCommand { paths } = parsed.classification {
        assert_eq!(
            paths,
            vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]
        );
    }
}

#[test]
fn parser_backtick_substitution_is_complex() {
    let parsed = parse_command("echo `whoami`");
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn parser_cargo_build_is_other() {
    let parsed = parse_command("cargo build");
    assert!(matches!(
        parsed.classification,
        CommandClassification::OtherSimple { .. }
    ));
}

#[test]
fn extract_paths_skips_long_flags() {
    let args: Vec<String> = vec!["--verbose".into(), "/tmp/file.txt".into()];
    let paths = extract_paths_from_args(&args);
    assert_eq!(paths, vec![PathBuf::from("/tmp/file.txt")]);
}

#[test]
fn extract_paths_multiple() {
    let args: Vec<String> = vec!["/tmp/a.txt".into(), "/tmp/b.txt".into()];
    let paths = extract_paths_from_args(&args);
    assert_eq!(
        paths,
        vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]
    );
}

#[test]
fn empty_command_is_complex() {
    let parsed = parse_command("");
    // An empty command may parse as an empty program — treat appropriately
    // The classification depends on what tree-sitter produces for empty input
    // It should either be complex or have no command name → complex fallback
    assert!(matches!(
        parsed.classification,
        CommandClassification::Complex
    ));
}

#[test]
fn stat_is_read() {
    let parsed = parse_command("stat /tmp/file");
    assert!(matches!(
        parsed.classification,
        CommandClassification::ReadCommand { .. }
    ));
}

#[test]
fn wc_is_read() {
    let parsed = parse_command("wc -l /tmp/file");
    assert!(matches!(
        parsed.classification,
        CommandClassification::ReadCommand { .. }
    ));
}

#[test]
fn md5sum_is_read() {
    let parsed = parse_command("md5sum /tmp/file");
    assert!(matches!(
        parsed.classification,
        CommandClassification::ReadCommand { .. }
    ));
}

#[test]
fn chmod_is_not_whitelisted() {
    let parsed = parse_command("chmod 755 /tmp/script.sh");
    assert!(matches!(
        parsed.classification,
        CommandClassification::OtherSimple { .. }
    ));
}

// ───────────────────────── try_decompose_complex tests ─────────────────────────

#[test]
fn decompose_simple_pipeline() {
    let d = try_decompose_complex("cat /etc/hosts | grep foo").expect("should decompose");
    assert_eq!(d.sub_commands, vec!["cat /etc/hosts", "grep foo"]);
    assert!(d.redirections.input_files.is_empty());
    assert!(d.redirections.output_files.is_empty());
}

#[test]
fn decompose_pipeline_with_fd_redirect() {
    // The most common pattern: `cargo check 2>&1 | tail -n 30`
    let d = try_decompose_complex("cargo check 2>&1 | tail -n 30").expect("should decompose");
    assert_eq!(d.sub_commands.len(), 2);
    assert_eq!(d.sub_commands[0], "cargo check 2>&1");
    assert_eq!(d.sub_commands[1], "tail -n 30");
}

#[test]
fn decompose_three_stage_pipeline() {
    let d = try_decompose_complex("cat /etc/hosts | grep foo | head -5").expect("should decompose");
    assert_eq!(d.sub_commands.len(), 3);
    assert_eq!(d.sub_commands[0], "cat /etc/hosts");
    assert_eq!(d.sub_commands[1], "grep foo");
    assert_eq!(d.sub_commands[2], "head -5");
}

#[test]
fn decompose_pipeline_with_output_redirect() {
    // `cat file | sort > out.txt` — redirect on the whole pipeline
    let d =
        try_decompose_complex("cat /etc/hosts | sort > /tmp/out.txt").expect("should decompose");
    assert_eq!(d.sub_commands.len(), 2);
    assert_eq!(
        d.redirections.output_files,
        vec![PathBuf::from("/tmp/out.txt")]
    );
}

#[test]
fn decompose_and_chain_no_state_mutation() {
    let d = try_decompose_complex("mkdir -p /tmp/test && touch /tmp/test/file")
        .expect("should decompose");
    assert_eq!(d.sub_commands.len(), 2);
    assert_eq!(d.sub_commands[0], "mkdir -p /tmp/test");
    assert_eq!(d.sub_commands[1], "touch /tmp/test/file");
}

#[test]
fn decompose_or_chain() {
    let d = try_decompose_complex("git pull || echo failed").expect("should decompose");
    assert_eq!(d.sub_commands.len(), 2);
}

#[test]
fn decompose_semicolon_chain() {
    let d = try_decompose_complex("echo a; echo b").expect("should decompose");
    assert_eq!(d.sub_commands.len(), 2);
    assert_eq!(d.sub_commands[0], "echo a");
    assert_eq!(d.sub_commands[1], "echo b");
}

#[test]
fn decompose_three_way_and_chain() {
    let d =
        try_decompose_complex("git stash && git pull && git stash pop").expect("should decompose");
    assert_eq!(d.sub_commands.len(), 3);
}

#[test]
fn reject_cd_in_and_chain() {
    assert!(try_decompose_complex("cd /tmp && ls").is_none());
}

#[test]
fn reject_pushd_in_chain() {
    assert!(try_decompose_complex("pushd /tmp && ls && popd").is_none());
}

#[test]
fn reject_export_in_chain() {
    assert!(try_decompose_complex("export FOO=bar && echo done").is_none());
}

#[test]
fn reject_source_in_chain() {
    assert!(try_decompose_complex("source ~/.bashrc && echo done").is_none());
}

#[test]
fn reject_dot_source_in_chain() {
    assert!(try_decompose_complex(". ~/.bashrc && echo done").is_none());
}

#[test]
fn reject_eval_in_chain() {
    assert!(try_decompose_complex("eval ls && echo done").is_none());
}

#[test]
fn reject_exec_in_chain() {
    assert!(try_decompose_complex("exec bash && echo done").is_none());
}

#[test]
fn reject_set_in_chain() {
    assert!(try_decompose_complex("set -e && echo done").is_none());
}

#[test]
fn reject_unset_in_chain() {
    assert!(try_decompose_complex("unset FOO && echo done").is_none());
}

#[test]
fn reject_alias_in_chain() {
    assert!(try_decompose_complex("alias ll='ls -la' && ll").is_none());
}

#[test]
fn reject_pipeline_with_command_substitution() {
    // `echo $(whoami) | grep root` — first stage has command substitution
    assert!(try_decompose_complex("echo $(whoami) | grep root").is_none());
}

#[test]
fn reject_pipeline_with_variable_expansion() {
    assert!(try_decompose_complex("echo $HOME | cat").is_none());
}

#[test]
fn reject_pipeline_with_subshell_stage() {
    assert!(try_decompose_complex("(echo a; echo b) | grep a").is_none());
}

#[test]
fn reject_and_chain_with_variable() {
    assert!(try_decompose_complex("FOO=bar && echo $FOO").is_none());
}

#[test]
fn decompose_returns_none_for_simple_command() {
    // A simple command should not be decomposed (it's not complex)
    assert!(try_decompose_complex("ls -la").is_none());
}

#[test]
fn decompose_returns_none_for_empty() {
    assert!(try_decompose_complex("").is_none());
}

#[test]
fn decompose_npm_install_and_build() {
    let d = try_decompose_complex("npm install && npm run build").expect("should decompose");
    assert_eq!(d.sub_commands.len(), 2);
    assert_eq!(d.sub_commands[0], "npm install");
    assert_eq!(d.sub_commands[1], "npm run build");
}

#[test]
fn decompose_git_diff_piped_to_head() {
    let d = try_decompose_complex("git diff HEAD | head -50").expect("should decompose");
    assert_eq!(d.sub_commands.len(), 2);
    assert_eq!(d.sub_commands[0], "git diff HEAD");
    assert_eq!(d.sub_commands[1], "head -50");
}

#[test]
fn decompose_and_chain_with_pipeline() {
    // Mixed: `cargo fmt && cargo clippy 2>&1 | tail -n 50`
    // AST: pipeline → redirected_statement(list("cargo fmt" && "cargo clippy"), 2>&1) | "tail -n 50"
    // In the AST, tree-sitter wraps the entire list in a `redirected_statement` with `2>&1`,
    // though in bash semantics the redirect only affects the pipeline's left-side output.
    // Since `2>&1` is a fd-to-fd redirect (not a file), it's irrelevant for permission checking.
    let d = try_decompose_complex("cargo fmt && cargo clippy 2>&1 | tail -n 50")
        .expect("should decompose");
    assert_eq!(d.sub_commands.len(), 3);
    assert_eq!(d.sub_commands[0], "cargo fmt");
    assert_eq!(d.sub_commands[1], "cargo clippy");
    assert_eq!(d.sub_commands[2], "tail -n 50");
}

#[test]
fn decompose_and_chain_with_redirected_pipeline() {
    // `echo start && cat /etc/hosts | sort > /tmp/out.txt`
    let d = try_decompose_complex("echo start && cat /etc/hosts | sort > /tmp/out.txt")
        .expect("should decompose");
    assert_eq!(d.sub_commands.len(), 3);
    assert_eq!(d.sub_commands[0], "echo start");
    assert_eq!(d.sub_commands[1], "cat /etc/hosts");
    assert_eq!(d.sub_commands[2], "sort");
    assert_eq!(
        d.redirections.output_files,
        vec![PathBuf::from("/tmp/out.txt")]
    );
}

#[test]
fn reject_cd_in_and_chain_with_pipeline() {
    // `cd /tmp && cargo check 2>&1 | tail` — cd is state-mutating, reject
    assert!(try_decompose_complex("cd /tmp && cargo check 2>&1 | tail").is_none());
}

#[test]
fn decompose_semicolon_with_pipeline() {
    // `echo start; cargo check 2>&1 | tail -n 30`
    let d = try_decompose_complex("echo start; cargo check 2>&1 | tail -n 30")
        .expect("should decompose");
    assert_eq!(d.sub_commands.len(), 3);
    assert_eq!(d.sub_commands[0], "echo start");
    assert_eq!(d.sub_commands[1], "cargo check 2>&1");
    assert_eq!(d.sub_commands[2], "tail -n 30");
}

#[test]
fn ampersand_redirect_to_file_is_output() {
    // &>filename redirects both stdout and stderr to a file
    let parsed = parse_command("echo hello &> /tmp/out.txt");
    assert_eq!(
        parsed.redirections.output_files,
        vec![PathBuf::from("/tmp/out.txt")]
    );
}

#[test]
fn redirect_ampersand_to_file_is_output() {
    // >&filename also redirects to a file (not fd-to-fd when target is not a number)
    let parsed = parse_command("echo hello >& /tmp/out.txt");
    assert_eq!(
        parsed.redirections.output_files,
        vec![PathBuf::from("/tmp/out.txt")]
    );
}

#[test]
fn fd_to_fd_redirect_still_skipped() {
    // 2>&1 is fd-to-fd, should not appear as a file redirect
    let parsed = parse_command("cargo build 2>&1");
    assert!(parsed.redirections.output_files.is_empty());
    assert!(parsed.redirections.input_files.is_empty());
}

#[test]
fn redirect_to_fd_number_is_not_file() {
    // >&2 redirects stdout to fd 2 — should be skipped
    let parsed = parse_command("echo error >&2");
    assert!(parsed.redirections.output_files.is_empty());
}

#[test]
fn reject_shopt_in_chain() {
    assert!(try_decompose_complex("shopt -s extglob && echo done").is_none());
}

#[test]
fn reject_trap_in_chain() {
    assert!(try_decompose_complex("trap 'echo bye' EXIT && echo done").is_none());
}
