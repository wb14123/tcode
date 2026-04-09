use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ts_dir = manifest_dir.join("../tree-sitter-tcode");

    // Determine the target directory (target/debug or target/release).
    // OUT_DIR is something like target/{profile}/build/tcode-{hash}/out,
    // so the profile directory is 3 levels up.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target_profile_dir = out_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or_else(|| {
            panic!(
                "Could not determine target profile directory from OUT_DIR: {}",
                out_dir.display()
            )
        });

    // Generate parser.c from grammar.js if missing or outdated.
    let grammar_js = ts_dir.join("grammar.js");
    let generated_dir = ts_dir.join("src/generated");
    let parser_c = generated_dir.join("parser.c");

    let needs_generate = !parser_c.exists() || {
        let grammar_mtime = std::fs::metadata(&grammar_js)
            .and_then(|m| m.modified())
            .ok();
        let parser_mtime = std::fs::metadata(&parser_c).and_then(|m| m.modified()).ok();
        match (grammar_mtime, parser_mtime) {
            (Some(g), Some(p)) => g > p,
            _ => true,
        }
    };

    if needs_generate {
        std::fs::create_dir_all(&generated_dir).expect("Failed to create src/generated directory");
        let status = Command::new("tree-sitter")
            .args(["generate", "-o", "src/generated", "grammar.js"])
            .current_dir(&ts_dir)
            .status()
            .expect("Failed to run `tree-sitter generate` — is tree-sitter CLI installed?");
        assert!(
            status.success(),
            "`tree-sitter generate` failed for tree-sitter-tcode"
        );
    }

    // Compile the shared library.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let lib_name = if target_os == "macos" {
        "libtree-sitter-tcode.dylib"
    } else {
        "libtree-sitter-tcode.so"
    };

    let shared_flag = if target_os == "macos" {
        "-dynamiclib"
    } else {
        "-shared"
    };

    let scanner_c = ts_dir.join("src/scanner.c");
    let include_dir = &generated_dir;
    let output_path = target_profile_dir.join(lib_name);

    let cc = env::var("CC").unwrap_or_else(|_| "cc".to_string());

    let status = Command::new(&cc)
        .args([
            shared_flag,
            "-O2",
            "-fPIC",
            "-I",
            include_dir.to_str().expect("include path must be UTF-8"),
            "-o",
            output_path.to_str().expect("output path must be UTF-8"),
            parser_c.to_str().expect("parser.c path must be UTF-8"),
            scanner_c.to_str().expect("scanner.c path must be UTF-8"),
        ])
        .status()
        .expect("Failed to run C compiler for tree-sitter-tcode");

    assert!(
        status.success(),
        "C compiler failed to build tree-sitter-tcode"
    );

    // Re-run if sources change.
    println!("cargo:rerun-if-changed={}", grammar_js.display());
    println!("cargo:rerun-if-changed={}", scanner_c.display());
    println!("cargo:rerun-if-changed={}", parser_c.display());
    println!("cargo:rerun-if-env-changed=CC");
}
