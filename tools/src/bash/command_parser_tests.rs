use std::path::PathBuf;

use super::command_parser::{
    CommandClassification, extract_paths_from_args, looks_like_path, parse_command,
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
