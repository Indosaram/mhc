use maho_cli::cli::{args::Args, help_fast_path::*};
#[test]
fn plain_help_excludes_print_and_modes() {
    let mut args = Args {
        help: true,
        ..Default::default()
    };
    assert!(is_plain_help_request(&args));
    args.print = true;
    assert!(!is_plain_help_request(&args));
}
#[test]
fn no_extension_help_needs_no_cache_and_trust_override_wins() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        cached_help_flags(
            &["--help".to_owned(), "--no-extensions".to_owned()],
            false,
            directory.path(),
            directory.path(),
            "1"
        ),
        Some(Vec::new())
    );
    let args = Args {
        project_trust_override: Some(false),
        ..Default::default()
    };
    assert!(
        !resolve_help_project_trust(
            &args,
            directory.path().to_str().unwrap(),
            directory.path().to_str().unwrap()
        )
        .unwrap()
    );
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
                let Some(end_idx) = after.find('"') else {
                    break;
                };
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

// senpi help-fast-path.ts `resolveHelpProjectTrust` / `helpFlagsScope`: a help screen trusts a
// project without trust-requiring resources, otherwise only a RECORDED decision lets it in.
#[test]
fn help_trust_reads_only_a_recorded_decision_and_the_scope_mirrors_the_launch() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("project");
    let agent = root.path().join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(
        cwd.join(maho_core::config::config_dir_name())
            .join("extensions"),
    )
    .unwrap();
    let (cwd_text, agent_text) = (cwd.to_str().unwrap(), agent.to_str().unwrap());
    let args = Args {
        help: true,
        extensions: vec!["./ext.js".to_owned()],
        ..Default::default()
    };
    assert!(
        !resolve_help_project_trust(&args, cwd_text, agent_text).unwrap(),
        "unrecorded project stays untrusted"
    );
    maho_core::trust_manager::ProjectTrustStore::new(agent_text)
        .set(cwd_text, Some(true))
        .unwrap();
    assert!(
        resolve_help_project_trust(&args, cwd_text, agent_text).unwrap(),
        "a recorded trust lets it in"
    );
    let override_args = Args {
        project_trust_override: Some(false),
        ..args.clone()
    };
    assert!(
        !resolve_help_project_trust(&override_args, cwd_text, agent_text).unwrap(),
        "the explicit override wins"
    );
    let scope = help_flags_scope(&args, &cwd, &agent, true);
    assert_eq!(
        scope.cli_extension_paths,
        vec![std::path::PathBuf::from("./ext.js")]
    );
    assert!(!scope.no_extensions && scope.project_trusted);
    assert_eq!((scope.cwd, scope.agent_dir), (cwd.clone(), agent.clone()));
}
