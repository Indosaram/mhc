use maho_cli::cli::host_command::*;
#[test]
fn host_options_parse_and_last_policy_wins() {
    let args = ["ensure", "--json", "--policy", "never", "--socket", "--literal", "--force"].map(str::to_owned);
    let parsed = parse_host_args(&args).unwrap();
    assert_eq!(parsed.subcommand, HostSubcommand::Ensure);
    assert_eq!(parsed.policy, "never");
    assert_eq!(parsed.socket.as_deref(), Some("--literal"));
    assert!(parsed.force);
}
#[test]
fn host_missing_or_invalid_policy_is_error() {
    assert!(parse_host_args(&[]).is_err());
    assert!(parse_host_args(&["ensure".to_owned(), "--policy".to_owned(), "invalid".to_owned()]).is_err());
}
