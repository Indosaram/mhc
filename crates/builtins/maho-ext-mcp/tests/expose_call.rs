//! Port of senpi mcp/expose/call.ts (v2026.10.27-2): `formatProgress` and the redacted call failure.
use maho_ext_mcp::{
    catalog::McpToolCatalogEntry,
    expose::call::{call_mcp_tool, format_progress},
};
use serde_json::json;
use std::time::Duration;

#[test]
fn progress_text_omits_an_undefined_total_and_message() {
    assert_eq!(
        format_progress("fx/tool_1", &json!({"progress":1})),
        "fx/tool_1 progress 1"
    );
    assert_eq!(
        format_progress(
            "fx/tool_1",
            &json!({"progress":2,"total":5,"message":"halfway"})
        ),
        "fx/tool_1 progress 2/5 halfway"
    );
}

#[tokio::test]
async fn a_call_without_any_connection_fails_as_a_tool_exec_error() {
    let entry = McpToolCatalogEntry {
        server: "fx".into(),
        tool: "tool_1".into(),
        schema: json!({"type":"object"}),
        output_schema: None,
        modern: false,
        description: None,
        annotations: None,
        request_timeout: Duration::from_secs(1),
        client: None,
        runtime: None,
        connection: None,
        ensure_connected: None,
        ensure_fresh: None,
        agent_dir: None,
        artifacts: None,
        output_guard: None,
        request_origin: None,
        invocation: None,
    };
    let error = call_mcp_tool(&entry, json!({}), "tc-1", &Default::default(), None)
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("ToolExecError:") && text.contains("MCP catalog entry has no connection"),
        "{text}"
    );
}
