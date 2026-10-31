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
// senpi v2026.10.27-2 host-command-args.ts: presenter flags (ensure/handoff only) and --rpc-key-root.
#[test]
fn presenter_flags_parse_only_for_ensure_and_handoff() {
    for sub in ["ensure", "handoff"] {
        let args = [sub, "--mcp-presenter-fd", "4", "--mcp-presenter-enforcement"].map(str::to_owned);
        let parsed = parse_host_args(&args).unwrap();
        assert_eq!(parsed.presenter_bootstrap_fd, Some(4));
        assert!(parsed.mcp_presenter_enforcement);
    }
    assert!(parse_host_args(&["status", "--mcp-presenter-enforcement"].map(str::to_owned)).is_err());
    assert!(parse_host_args(&["ensure", "--mcp-presenter-fd", "3"].map(str::to_owned)).is_err());
    let parsed = parse_host_args(&["ensure".to_owned()]).unwrap();
    assert_eq!(parsed.presenter_bootstrap_fd, None);
    assert!(!parsed.mcp_presenter_enforcement);
}
#[test]
fn rpc_key_root_resolves_to_an_absolute_path_on_every_subcommand() {
    let parsed = parse_host_args(&["status", "--rpc-key-root", "keys"].map(str::to_owned)).unwrap();
    let root = parsed.key_root.expect("key root");
    assert!(std::path::Path::new(&root).is_absolute());
    assert!(root.ends_with("keys"));
    assert!(parse_host_args(&["status", "--rpc-key-root", "--json"].map(str::to_owned)).is_err());
    assert!(parse_host_args(&["status".to_owned()]).unwrap().key_root.is_none());
}
// Until maho-rpc's host runner takes request options, a request naming them is refused, never run without them.
#[test]
fn request_options_the_runner_cannot_carry_are_refused_by_name() {
    let plain = parse_host_args(&["ensure".to_owned()]).unwrap();
    assert!(maho_cli::cli::host_request::unsupported_request_options(&plain).is_none());
    let named = parse_host_args(
        &[
            "ensure",
            "--mcp-presenter-enforcement",
            "--rpc-key-root",
            "keys",
        ]
        .map(str::to_owned),
    )
    .unwrap();
    let refused =
        maho_cli::cli::host_request::unsupported_request_options(&named).expect("refused");
    assert_eq!(refused.exit_code(), 1);
    let detail = refused.payload()["detail"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(detail.contains("--rpc-key-root"));
    assert!(detail.contains("--mcp-presenter-enforcement"));
    assert!(!detail.contains("--mcp-presenter-fd"));
}
