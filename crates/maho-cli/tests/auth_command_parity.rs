use maho_cli::cli::{args::parse_args, auth_command::{parse_auth_command, validate_auth_command_args, AuthCommandKind}};

#[test]
fn auth_unknown_option_is_reported_by_actual_cli() {
    let root = tempfile::tempdir().expect("home");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .env_clear().env("HOME", root.path()).env("PATH", "/usr/bin:/bin")
        .args(["auth", "check", "--provider", "chatgpt-subscription", "--credentails"])
        .output().expect("CLI");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8(output.stderr).expect("stderr").contains("Unknown option --credentails for \"auth check\"."));
}

#[test]
fn credential_commands_parse_and_reject_invalid_arguments() {
    let args = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>();
    let command = parse_auth_command(&args(&["auth", "print-api-key", "--provider", "openai"])).expect("parse").expect("command");
    assert!(command.kind == AuthCommandKind::ApiKey);
    assert_eq!(command.args, args(&["--provider", "openai"]));
    assert!(!command.json && !command.credentials && !command.no_refresh);
    assert!(parse_auth_command(&args(&["auth", "unknown"])).is_err());
    assert!(parse_auth_command(&args(&["auth", "print-api-key", "--min-expiry", "30m"])).is_err());
    assert!(validate_auth_command_args(&parse_args(&[], false).expect("args"), AuthCommandKind::ApiKey).is_err());
}
