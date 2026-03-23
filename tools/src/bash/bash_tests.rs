#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::bash::command_parser::{
        CommandClassification, extract_paths_from_args, parse_command,
    };
    use crate::bash::command_permission::command_matches_permission;

    // ---- Command Parser Tests ----

    #[test]
    fn parser_cat_is_read() {
        let parsed = parse_command("cat /etc/passwd");
        assert!(matches!(
            parsed.classification,
            CommandClassification::ReadCommand { .. }
        ));
    }

    #[test]
    fn parser_head_with_flags_is_read() {
        let parsed = parse_command("head -n 5 /var/log/syslog");
        assert!(matches!(
            parsed.classification,
            CommandClassification::ReadCommand { .. }
        ));
        if let CommandClassification::ReadCommand { paths } = parsed.classification {
            assert_eq!(paths, vec![PathBuf::from("/var/log/syslog")]);
        }
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
    fn parser_mkdir_is_write() {
        let parsed = parse_command("mkdir -p /tmp/my/new/dir");
        assert!(matches!(
            parsed.classification,
            CommandClassification::WriteCommand { .. }
        ));
        if let CommandClassification::WriteCommand { paths } = parsed.classification {
            assert_eq!(paths, vec![PathBuf::from("/tmp/my/new/dir")]);
        }
    }

    #[test]
    fn parser_touch_is_write() {
        let parsed = parse_command("touch /tmp/newfile");
        assert!(matches!(
            parsed.classification,
            CommandClassification::WriteCommand { .. }
        ));
    }

    #[test]
    fn parser_git_is_other() {
        let parsed = parse_command("git status");
        assert!(matches!(
            parsed.classification,
            CommandClassification::OtherSimple { .. }
        ));
        if let CommandClassification::OtherSimple { tokens } = parsed.classification {
            assert_eq!(tokens[0], "git");
            assert_eq!(tokens[1], "status");
        }
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
    fn parser_rm_is_other_not_whitelisted() {
        let parsed = parse_command("rm -rf /tmp/something");
        assert!(matches!(
            parsed.classification,
            CommandClassification::OtherSimple { .. }
        ));
    }

    #[test]
    fn parser_pipe_is_complex() {
        let parsed = parse_command("ps aux | grep rust");
        assert!(matches!(
            parsed.classification,
            CommandClassification::Complex
        ));
    }

    #[test]
    fn parser_and_chain_is_complex() {
        let parsed = parse_command("cargo build && cargo test");
        assert!(matches!(
            parsed.classification,
            CommandClassification::Complex
        ));
    }

    #[test]
    fn parser_semicolon_is_complex() {
        let parsed = parse_command("echo hello; echo world");
        assert!(matches!(
            parsed.classification,
            CommandClassification::Complex
        ));
    }

    #[test]
    fn parser_command_substitution_is_complex() {
        let parsed = parse_command("echo $(date)");
        assert!(matches!(
            parsed.classification,
            CommandClassification::Complex
        ));
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
    fn parser_subshell_is_complex() {
        let parsed = parse_command("(cd /tmp && ls)");
        assert!(matches!(
            parsed.classification,
            CommandClassification::Complex
        ));
    }

    #[test]
    fn parser_variable_expansion_is_complex() {
        let parsed = parse_command("$HOME/bin/tool arg");
        assert!(matches!(
            parsed.classification,
            CommandClassification::Complex
        ));
    }

    // ---- Redirection Tests ----

    #[test]
    fn redirect_output() {
        let parsed = parse_command("echo hello > /tmp/out.txt");
        assert_eq!(
            parsed.redirections.output_files,
            vec![PathBuf::from("/tmp/out.txt")]
        );
    }

    #[test]
    fn redirect_append() {
        let parsed = parse_command("echo hello >> /tmp/log.txt");
        assert_eq!(
            parsed.redirections.output_files,
            vec![PathBuf::from("/tmp/log.txt")]
        );
    }

    #[test]
    fn redirect_input() {
        let parsed = parse_command("wc -l < /tmp/data.txt");
        assert_eq!(
            parsed.redirections.input_files,
            vec![PathBuf::from("/tmp/data.txt")]
        );
    }

    #[test]
    fn redirect_read_command_with_output() {
        // cat is a read command, but has output redirect — both should be extracted
        let parsed = parse_command("cat /etc/hosts > /tmp/copy");
        assert!(matches!(
            parsed.classification,
            CommandClassification::ReadCommand { .. }
        ));
        if let CommandClassification::ReadCommand { paths } = &parsed.classification {
            assert_eq!(paths, &[PathBuf::from("/etc/hosts")]);
        }
        assert_eq!(
            parsed.redirections.output_files,
            vec![PathBuf::from("/tmp/copy")]
        );
    }

    // ---- Path Extraction Tests ----

    #[test]
    fn extract_paths_skips_short_flags() {
        let args: Vec<String> = vec!["-n".into(), "10".into(), "/etc/hosts".into()];
        let paths = extract_paths_from_args(&args);
        assert_eq!(paths, vec![PathBuf::from("/etc/hosts")]);
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

    // ---- Permission Matching Tests ----

    #[test]
    fn permission_exact_match() {
        assert!(command_matches_permission("git", "git"));
        assert!(command_matches_permission("cargo", "cargo"));
    }

    #[test]
    fn permission_prefix_with_space() {
        assert!(command_matches_permission("git", "git diff"));
        assert!(command_matches_permission("git", "git add ."));
        assert!(command_matches_permission("cargo", "cargo build"));
    }

    #[test]
    fn permission_no_partial_word_match() {
        assert!(!command_matches_permission("git", "gitabc"));
        assert!(!command_matches_permission("cargo", "cargoabc"));
    }

    #[test]
    fn permission_subcommand_match() {
        assert!(command_matches_permission(
            "git push",
            "git push origin main"
        ));
        assert!(!command_matches_permission("git push", "git add ."));
    }

    #[test]
    fn permission_npm_match() {
        assert!(command_matches_permission("npm", "npm install"));
        assert!(command_matches_permission("npm", "npm run build"));
        assert!(!command_matches_permission("npm", "npx create"));
    }

    // ---- Edge Cases ----

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
    fn single_command_no_args() {
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
}
