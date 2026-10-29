//! Acceptance tests 3, 4, 5, 6, 8 and 27 of `docs/cleanroom/specs/ast-grep.md` §8, plus the
//! command line of §1.

mod common;

use common::{call, code_of, server_with};
use maho_ast_grep::resolve::Toolchain;
use maho_ast_grep::{abort_channel, handle_message, idle_timeout, run_cli};
use serde_json::{Value, json};
use std::time::Duration;

async fn rpc(server: &maho_ast_grep::Server, line: &str) -> Option<Value> {
    let (_handle, mut signal) = abort_channel();
    tokio::time::timeout(Duration::from_secs(30), handle_message(server, line, &mut signal))
        .await
        .expect("reply within bound")
}

fn empty_server() -> maho_ast_grep::Server {
    server_with(
        Toolchain {
            platform: "linux".into(),
            arch: "x64".into(),
            ..Toolchain::default()
        },
        None,
    )
}

fn has_composition_keyword(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, inner)| {
            matches!(key.as_str(), "oneOf" | "anyOf" | "allOf" | "not") || has_composition_keyword(inner)
        }),
        Value::Array(items) => items.iter().any(has_composition_keyword),
        _ => false,
    }
}

#[tokio::test]
async fn t3_initialize_defaults_and_echoes_the_version() {
    let server = empty_server();
    let reply = rpc(&server, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .await
        .expect("reply");
    assert_eq!(reply["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(reply["result"]["serverInfo"]["name"], "ast_grep");
    assert_eq!(reply["result"]["capabilities"]["tools"]["listChanged"], false);
    let reply = rpc(
        &server,
        r#"{"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2099-01-01"}}"#,
    )
    .await
    .expect("reply");
    assert_eq!(reply["result"]["protocolVersion"], "2099-01-01");
}

#[tokio::test]
async fn t4_tools_list_has_three_tools_without_composition_keywords() {
    let server = empty_server();
    let reply = rpc(&server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
        .await
        .expect("reply");
    let tools = reply["result"]["tools"].as_array().expect("tools");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names, ["search", "rewrite", "scan"]);
    for tool in tools {
        assert!(!has_composition_keyword(&tool["inputSchema"]), "{}", tool["name"]);
    }
}

#[tokio::test]
async fn t5_protocol_requests_and_errors() {
    let server = empty_server();
    let ping = rpc(&server, r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#).await.expect("reply");
    assert_eq!(ping["result"], json!({}));
    assert!(
        rpc(&server, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .is_none()
    );
    let array = rpc(&server, "[1]").await.expect("reply");
    assert_eq!(array["error"]["code"], -32600);
    let unknown = rpc(&server, r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#).await.expect("reply");
    assert_eq!(unknown["error"]["code"], -32601);
    let nameless = rpc(&server, r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{}}"#)
        .await
        .expect("reply");
    assert_eq!(nameless["error"]["code"], -32602);
    let broken = rpc(&server, "{not json").await.expect("reply");
    assert_eq!(broken["error"]["code"], -32700);
}

#[tokio::test]
async fn t6_without_the_program_initialize_works_and_search_fails_with_hints() {
    let server = empty_server();
    let init = rpc(&server, r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).await.expect("reply");
    assert!(init["result"]["serverInfo"].is_object());
    let dir = tempfile::tempdir().expect("tempdir");
    let (value, is_error) = call(
        &server,
        "search",
        json!({ "paths": ["."], "pattern": "foo($A)", "language": "javascript", "workdir": dir.path() }),
    )
    .await;
    assert!(is_error);
    assert_eq!(value["ok"], false);
    assert_eq!(code_of(&value), "BINARY_NOT_FOUND");
    let hints = value["error"]["details"]["hints"].as_array().expect("hints");
    assert!(!hints.is_empty());
}

#[tokio::test]
async fn t8_invalid_arguments_fail_in_preflight() {
    let server = empty_server();
    let (empty, is_error) = call(&server, "search", json!({})).await;
    assert!(is_error);
    assert_eq!(code_of(&empty), "INVALID_ARGUMENT");
    assert_eq!(empty["error"]["phase"], "preflight");

    let (unknown, _) = call(
        &server,
        "search",
        json!({ "paths": ["."], "pattern": "a", "language": "javascript", "bogus": 1 }),
    )
    .await;
    assert_eq!(code_of(&unknown), "INVALID_ARGUMENT");
    assert_eq!(unknown["error"]["phase"], "preflight");

    // 8193 two-byte characters: fewer than 16385 characters, but 16386 bytes.
    let long = "é".repeat(8193);
    let (too_long, _) = call(
        &server,
        "search",
        json!({ "paths": ["."], "pattern": long, "language": "javascript" }),
    )
    .await;
    assert_eq!(code_of(&too_long), "INVALID_ARGUMENT");
    assert_eq!(too_long["error"]["phase"], "preflight");

    let (bad_language, _) = call(
        &server,
        "search",
        json!({ "paths": ["."], "pattern": "a", "language": "cobol" }),
    )
    .await;
    assert_eq!(code_of(&bad_language), "UNSUPPORTED_LANGUAGE");
}

#[tokio::test]
async fn unknown_tool_lists_the_available_ones() {
    let server = empty_server();
    let (value, is_error) = call(&server, "frobnicate", json!({})).await;
    assert!(is_error);
    assert_eq!(code_of(&value), "INVALID_ARGUMENT");
    assert_eq!(value["error"]["phase"], "preflight");
    assert!(value["error"]["message"].as_str().unwrap_or_default().contains("search"));
}

#[test]
fn t27_idle_timeout_values() {
    let env = |value: Option<&'static str>| {
        move |name: &str| {
            (name == "MHC_AST_GREP_IDLE_TIMEOUT_MS")
                .then(|| value.map(str::to_owned))
                .flatten()
        }
    };
    let default = Some(Duration::from_millis(600_000));
    assert_eq!(idle_timeout(env(None)), default);
    assert_eq!(idle_timeout(env(Some("soon"))), default);
    assert_eq!(idle_timeout(env(Some(""))), default);
    assert_eq!(idle_timeout(env(Some("inf"))), default);
    assert_eq!(idle_timeout(env(Some("-5"))), None);
    assert_eq!(idle_timeout(env(Some("0"))), None);
    assert_eq!(idle_timeout(env(Some("2500"))), Some(Duration::from_millis(2500)));
    let legacy = |name: &str| (name == "OMO_AST_GREP_IDLE_TIMEOUT_MS").then(|| "1000".to_owned());
    assert_eq!(idle_timeout(legacy), Some(Duration::from_millis(1000)));
}

#[tokio::test]
async fn other_first_argument_prints_usage_and_exits_2() {
    let server = empty_server();
    let mut stderr: Vec<u8> = Vec::new();
    let code = run_cli(
        &["serve".to_owned()],
        &server,
        tokio::io::BufReader::new(tokio::io::empty()),
        tokio::io::sink(),
        &mut stderr,
        std::future::pending::<()>(),
    )
    .await;
    assert_eq!(code, 2);
    assert!(String::from_utf8_lossy(&stderr).contains("usage"));
}

#[tokio::test]
async fn mcp_argument_serves_until_stdin_closes() {
    let server = empty_server();
    let mut stderr: Vec<u8> = Vec::new();
    let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n".as_slice();
    let (client, mut reader) = tokio::io::duplex(4096);
    let code = run_cli(
        &["mcp".to_owned()],
        &server,
        tokio::io::BufReader::new(input),
        client,
        &mut stderr,
        std::future::pending::<()>(),
    )
    .await;
    assert_eq!(code, 0);
    let mut out = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut reader, &mut out).await.expect("read");
    assert!(out.contains("\"result\":{}"));
}
