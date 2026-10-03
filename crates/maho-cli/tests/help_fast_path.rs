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
