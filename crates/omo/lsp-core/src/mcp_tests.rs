use super::*;
use mcp_stdio_core::watchdog::ParentWatchdogConfig;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[tokio::test]
async fn initialize_response_keeps_exact_server_info_and_capabilities() {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "todo-23", "version": "0.0.0" },
        },
    });

    let response = handle_lsp_mcp_request(&request, HandleLspMcpRequestOptions::default()).await;

    assert_eq!(
        serde_json::to_value(response).expect("serialize"),
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "lsp", "version": "0.1.0" },
                "protocolVersion": "2024-11-05",
            },
        })
    );
}

#[test]
fn malformed_stdio_line_yields_parse_error_with_parser_data() {
    let mut output: Vec<u8> = Vec::new();
    let options = LspMcpStdioServerOptions {
        parent_watchdog: Some(ParentWatchdogConfig {
            parent_pid: Some(1),
            poll_interval_ms: Some(0),
            probe_alive: Some(Arc::new(|_| true)),
        }),
    };

    run_mcp_stdio_server(
        std::io::Cursor::new(b"garbage\n".to_vec()),
        &mut output,
        options,
    )
    .expect("server runs");

    let response: Value = serde_json::from_slice(&output).expect("json response");
    let data = response["error"]["data"].clone();
    assert_eq!(
        response,
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32700, "message": "Parse error", "data": data },
        })
    );
    assert!(data.as_str().expect("string data").contains("garbage"));
}

#[tokio::test]
async fn protocol_dispatch_covers_invalid_ping_notification_list_and_unknown_method() {
    let options = HandleLspMcpRequestOptions::default;
    let invalid = handle_lsp_mcp_request(&json!([1]), options()).await;
    let ping = handle_lsp_mcp_request(&json!({ "id": "a", "method": "ping" }), options()).await;
    let notification =
        handle_lsp_mcp_request(&json!({ "method": "notifications/initialized" }), options()).await;
    let default_version =
        handle_lsp_mcp_request(&json!({ "id": 2, "method": "initialize" }), options()).await;
    let list = handle_lsp_mcp_request(&json!({ "id": 3, "method": "tools/list" }), options()).await;
    let unknown = handle_lsp_mcp_request(&json!({ "id": 4, "method": "nope" }), options()).await;
    let no_name = handle_lsp_mcp_request(
        &json!({ "id": 5, "method": "tools/call", "params": {} }),
        options(),
    )
    .await;
    let bad_tool = handle_lsp_mcp_request(
        &json!({ "id": 6, "method": "tools/call", "params": { "name": "bogus" } }),
        options(),
    )
    .await;

    let to_json =
        |response: Option<JsonRpcResponse>| serde_json::to_value(response).expect("serialize");
    assert_eq!(
        to_json(invalid),
        json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "Invalid Request" } })
    );
    assert_eq!(
        to_json(ping),
        json!({ "jsonrpc": "2.0", "id": "a", "result": {} })
    );
    assert_eq!(notification, None);
    assert_eq!(
        to_json(default_version)["result"]["protocolVersion"],
        json!("2024-11-05")
    );
    let names: Vec<Value> = to_json(list)["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].clone())
        .collect();
    assert_eq!(
        names,
        vec![
            json!("status"),
            json!("diagnostics"),
            json!("goto_definition"),
            json!("find_references"),
            json!("symbols"),
            json!("prepare_rename"),
            json!("rename"),
            json!("install_decision"),
        ]
    );
    assert_eq!(
        to_json(unknown)["error"],
        json!({ "code": -32601, "message": "Method not found: nope" })
    );
    assert_eq!(
        to_json(no_name)["error"],
        json!({ "code": -32602, "message": "tools/call requires params.name" })
    );
    assert_eq!(
        to_json(bad_tool)["result"],
        json!({ "content": [{ "type": "text", "text": "Unknown LSP tool: bogus" }], "isError": true })
    );
}
