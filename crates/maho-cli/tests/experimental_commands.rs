use maho_cli::cli::experimental::{command_options::{parse_transport_address, TransportAddress}, commands::{client::parse_client, server::parse_server}};
fn args(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }
#[test]
fn unix_addresses_decode_paths_and_reject_authorities() {
    assert!(matches!(parse_transport_address("unix:///tmp/test%20socket"), Ok(TransportAddress::Unix(path)) if path == "/tmp/test socket"));
    for value in ["unix://host/tmp/test", "unix:////tmp/test", "unix:///tmp/%00", "unix:///tmp/%zz", "unix:///tmp/test?query"] { assert!(parse_transport_address(value).is_err()); }
}
#[test]
fn radius_requires_lowercase_v4_identity() {
    assert!(parse_transport_address("radius://01234567-89ab-4cde-8fab-0123456789ab").is_ok());
    for value in ["radius://01234567-89ab-5cde-8fab-0123456789ab", "radius://01234567-89AB-4cde-8fab-0123456789ab", "radius://01234567-89ab-4cde-8fab-0123456789ab/path"] { assert!(parse_transport_address(value).is_err()); }
}
#[test]
fn client_accepts_single_prompt_and_repeatable_plugins() {
    let command = parse_client(&args(&["-e", "first", "-e=second", "--model", "model", "--", "--prompt"])).ok().unwrap();
    assert_eq!(command.plugin_packages, args(&["first", "second"]));
    assert_eq!(command.prompt.as_deref(), Some("--prompt"));
}
#[test]
fn client_rejects_conflicting_sessions_and_provider_without_model() {
    let errors = parse_client(&args(&["--session-id", "session", "-c", "--provider", "provider"])).err().unwrap();
    assert_eq!(errors.len(), 2);
}
#[test]
fn client_rejects_multiple_prompts_and_conflicting_auth() {
    assert!(parse_client(&args(&["one", "two"])).is_err());
    assert!(parse_client(&args(&["--auth-token", "test-token", "--auth-token-file", "file"])).is_err());
}
#[test]
fn server_accepts_identifier_and_model_but_not_prompts() {
    let command = parse_server(&args(&["--server-id", "01234567-89ab-4cde-8fab-0123456789ab", "--model", "model", "--session-dir", "/tmp/sessions"])).ok().unwrap();
    assert_eq!(command.session_dir.as_deref(), Some("/tmp/sessions"));
    assert!(parse_server(&args(&["--server-id", "invalid"])).is_err());
    assert!(parse_server(&args(&["prompt"])).is_err());
}
