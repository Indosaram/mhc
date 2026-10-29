use maho_cli::cli::{args::Args, help_fast_path::*};
#[test]
fn plain_help_excludes_print_and_modes() {
    let mut args = Args { help: true, ..Default::default() };
    assert!(is_plain_help_request(&args));
    args.print = true;
    assert!(!is_plain_help_request(&args));
}
#[test]
fn no_extension_help_needs_no_cache_and_trust_override_wins() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(cached_help_flags(&["--help".to_owned(), "--no-extensions".to_owned()], false, directory.path(), directory.path(), "1"), Some(Vec::new()));
    let args = Args { project_trust_override: Some(false), ..Default::default() };
    assert!(!resolve_help_project_trust(&args, directory.path().to_str().unwrap(), directory.path().to_str().unwrap()).unwrap());
}

const ALL_SUBCOMMANDS: &[&str] = &[
    "install",
    "remove",
    "uninstall",
    "update",
    "list",
    "config",
    "host",
    "auth",
    "doctor",
    "import-omo",
    "ulw-loop",
    "schedule",
    "models discover",
    "senpi-install",
    "mini",
];

fn scan_dispatch_subcommand_match_arms(main_src: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in main_src.lines() {
        for marker in ["Some(\"", "arg == \""] {
            let mut rest = line;
            while let Some(idx) = rest.find(marker) {
                let after = &rest[idx + marker.len()..];
                let Some(end_idx) = after.find('"') else { break };
                let name = &after[..end_idx];
                if !name.is_empty() && !name.starts_with("--") && !names.iter().any(|n| n == name) {
                    names.push(name.to_owned());
                }
                rest = &after[end_idx + 1..];
            }
        }
    }
    names
}

fn assert_subcommands_in_help(help_text: &str, names: &[&str]) -> Result<(), String> {
    for name in names {
        if !help_text.contains(name) {
            return Err(format!("help.txt is missing subcommand: {name}"));
        }
    }
    Ok(())
}

#[test]
fn help_text_covers_all_dispatch_match_arms() {
    let main_src = include_str!("../src/main.rs");
    let help_text = include_str!("../src/cli/help.txt");

    let scanned_arms = scan_dispatch_subcommand_match_arms(main_src);
    assert!(
        !scanned_arms.is_empty(),
        "Expected to find Some(\"<name>\") or arg == \"<name>\" patterns in main.rs dispatch"
    );

    let arm_refs: Vec<&str> = scanned_arms.iter().map(String::as_str).collect();
    if let Err(missing) = assert_subcommands_in_help(help_text, &arm_refs) {
        panic!("{missing}");
    }

    if let Err(missing) = assert_subcommands_in_help(help_text, ALL_SUBCOMMANDS) {
        panic!("{missing}");
    }
}

#[test]
fn help_test_fails_naming_missing_entry() {
    let help_text = include_str!("../src/cli/help.txt");

    for subcommand in ALL_SUBCOMMANDS {
        let scratch_help = help_text.replace(subcommand, "");
        let err = assert_subcommands_in_help(&scratch_help, ALL_SUBCOMMANDS)
            .expect_err(&format!("expected failure when missing {subcommand}"));
        assert!(
            err.contains(subcommand),
            "Expected error to name missing subcommand '{subcommand}', got '{err}'"
        );
    }
}
