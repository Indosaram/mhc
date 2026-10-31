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

// senpi credential-print.test.ts and auth-check.test.ts parseAuthCommand / isAuthCommandHelp cases.
#[test]
fn upstream_bearer_min_expiry_check_flags_and_help_recognition() {
    use maho_cli::cli::auth_command::is_auth_command_help;
    let args = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect::<Vec<_>>();
    let bearer = parse_auth_command(&args(&["auth", "print-bearer-token"])).expect("parse").expect("command");
    assert!(bearer.kind == AuthCommandKind::BearerToken);
    assert_eq!(bearer.min_expiry_ms, None);
    let expiring = parse_auth_command(&args(&["auth", "print-bearer-token", "--min-expiry", "30m"])).expect("parse").expect("command");
    assert!(expiring.args.is_empty());
    assert_eq!(expiring.min_expiry_ms, Some(30 * 60_000));
    let error = parse_auth_command(&args(&["auth", "print-api-key", "--min-expiry", "30m"])).err().expect("refused");
    assert!(error.contains("only supported by print-bearer-token"), "{error}");
    let check = parse_auth_command(&args(&["auth", "check", "--provider", "openai"])).expect("parse").expect("command");
    assert!(check.kind == AuthCommandKind::Check);
    assert_eq!(check.args, args(&["--provider", "openai"]));
    assert!(!check.json && !check.credentials && !check.no_refresh);
    let flagged = parse_auth_command(&args(&["auth", "check", "--json", "--credentials", "--no-refresh", "--provider", "openai"])).expect("parse").expect("command");
    assert_eq!(flagged.args, args(&["--provider", "openai"]));
    assert!(flagged.json && flagged.credentials && flagged.no_refresh);
    for help in [&["auth", "--help"][..], &["auth", "print-api-key", "--help"], &["auth", "print-bearer-token", "-h"], &["auth", "check", "--help"]] {
        assert!(is_auth_command_help(&args(help)), "{help:?}");
    }
    assert!(parse_auth_command(&args(&["login"])).expect("not auth").is_none());
}
