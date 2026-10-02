fn run(args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().expect("isolated CLI directory");
    std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path()).env("HOME", dir.path()).env_remove("__PI_INTERNAL_SPAWN")
        .args(args).output().expect("run real CLI")
}
#[test]
fn real_entry_rejects_rpc_attachments_before_mode_dispatch() {
    let result = run(&["--mode", "rpc", "@missing.txt"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8(result.stderr).unwrap().contains("@file arguments are not supported in RPC mode"));
}
#[test]
fn real_entry_rejects_session_conflicts_before_mode_dispatch() {
    let result = run(&["--fork", "source", "--continue"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8(result.stderr).unwrap().contains("--fork cannot be combined with --continue"));
    let result = run(&["--session-id", "bad/id"]);
    assert_eq!(result.status.code(), Some(1));
    assert!(String::from_utf8(result.stderr).unwrap().contains("Session id must be non-empty"));
}

#[test]
fn real_rpc_entry_dispatches_message_query_and_preserves_correlation() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("isolated CLI directory");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_mhc"))
        .current_dir(dir.path()).env("HOME", dir.path())
        .env("MAHO_CODING_AGENT_DIR", dir.path().join("agent"))
        .env_remove("__PI_INTERNAL_SPAWN")
        .args(["--mode", "rpc", "--offline", "--no-session", "--model", "openai/gpt-4o"])
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
        .spawn().expect("real RPC process");
    let mut input = child.stdin.take().expect("stdin");
    input.write_all(b"{\"id\":\"query\",\"type\":\"get_messages\"}\n").expect("command");
    drop(input);
    let output = child.wait_with_output().expect("RPC exit");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON response");
    assert_eq!(response["id"], "query");
    assert_eq!(response["command"], "get_messages");
    assert_eq!(response["success"], true);
    assert_eq!(response["data"]["messages"], serde_json::json!([]));
}

#[test]
fn native_startup_registers_shipped_api_streams() {
    maho_cli::cli::setup::register_builtin_apis();
    for api in ["anthropic-messages", "openai-completions", "openai-responses", "google-generative-ai", "cursor-agent"] {
        assert!(maho_ai::api_registry::get_builtin_api_provider(api).is_some(), "{api}");
    }
}
