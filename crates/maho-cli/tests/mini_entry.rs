use maho_cli::experimental::mini::entry::{parse_tui_args, parse_server_args, parse_worker_args};
fn args(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }
#[test]
fn presentation_accepts_only_continue_aliases() {
    assert!(!parse_tui_args(&[]).unwrap().continue_session);
    for alias in ["--continue", "-c"] { assert!(parse_tui_args(&args(&[alias])).unwrap().continue_session); }
    assert_eq!(parse_tui_args(&args(&["--help"])).unwrap_err(), "Unknown argument: --help");
}
#[test]
fn server_entry_preserves_positional_contract() {
    assert!(parse_server_args(&[]).is_err());
    assert!(parse_server_args(&args(&["", "/sessions"])).is_err());
    let options = parse_server_args(&args(&["/socket", "/sessions", "ignored"])).unwrap();
    assert_eq!(options.socket_path, "/socket"); assert_eq!(options.sessions_root, "/sessions");
}
#[test]
fn worker_entry_preserves_optional_session_identity() {
    assert!(parse_worker_args(&args(&["/sessions"])).is_err());
    assert_eq!(parse_worker_args(&args(&["/sessions", "/cwd"])).unwrap().session_id, None);
    let options = parse_worker_args(&args(&["/sessions", "/cwd", "id", "ignored"])).unwrap();
    assert_eq!(options.session_id.as_deref(), Some("id")); assert_eq!(options.cwd, "/cwd");
}
