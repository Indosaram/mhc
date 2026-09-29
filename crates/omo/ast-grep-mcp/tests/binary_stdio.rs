mod common;

use std::io::Write;
use std::process::Command;
use std::process::Stdio;

use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn run_binary(lines: &[Value], sg_path_override: Option<&str>) -> Vec<Value> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ast-grep-mcp"));
    command
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = sg_path_override {
        command.env("OMO_AST_GREP_SG_PATH", path);
    }
    let mut child = command.spawn().expect("spawn ast-grep-mcp");
    let mut stdin = child.stdin.take().expect("stdin");
    for line in lines {
        writeln!(stdin, "{line}").expect("write request");
    }
    drop(stdin);
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success(), "exit status {:?}", output.status);
    String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect()
}

fn payload_of(response: &Value) -> Value {
    serde_json::from_str(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("text"),
    )
    .expect("payload")
}

#[test]
fn binary_serves_handshake_list_and_domain_errors_over_stdio() {
    let responses = run_binary(
        &[
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "nope", "arguments": {} } }),
        ],
        None,
    );
    assert_eq!(responses.len(), 3);
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        json!("ast_grep")
    );
    assert_eq!(
        responses[0]["result"]["protocolVersion"],
        json!("2025-06-18")
    );
    let names: Vec<&str> = responses[1]["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, vec!["search", "rewrite", "scan"]);
    assert_eq!(responses[2]["result"]["isError"], json!(true));
    assert_eq!(
        payload_of(&responses[2])["error"]["code"],
        json!("INVALID_ARGUMENT")
    );
}

#[test]
fn binary_dispatches_search_to_real_sg_over_stdio() {
    if !common::sg_available() {
        eprintln!("skipping: {} not installed", common::SG_PATH);
        return;
    }
    let repo = common::fixture_repo();
    let responses = run_binary(
        &[json!({
            "jsonrpc": "2.0", "id": "s", "method": "tools/call",
            "params": { "name": "search", "arguments": {
                "pattern": "console.log($MSG)", "language": "typescript",
                "paths": ["src"], "workdir": common::path_str(repo.path()),
            } },
        })],
        Some(common::SG_PATH),
    );
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["result"]["isError"], json!(false));
    let payload = payload_of(&responses[0]);
    assert_eq!(payload["ok"], json!(true));
    let paths: Vec<&str> = payload["matches"]
        .as_array()
        .expect("matches")
        .iter()
        .filter_map(|entry| entry["path"].as_str())
        .collect();
    assert_eq!(paths, vec!["src/a.ts", "src/a.ts", "src/a.ts", "src/b.ts"]);
}
