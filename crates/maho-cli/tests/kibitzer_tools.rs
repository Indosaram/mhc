//! Registered `ToolDefinition` execution regressions for the CLI host-facing Kibitzer tools. Each
//! case builds the REAL definitions and runs them through `ToolDefinition::execute`, so the binding
//! (params -> shared executor, result -> `ToolResult`/`ToolError`, per-session budget) is what is
//! proven. The budget slot is the CLI's `CliBudgetSlot`, the SAME producer trait the mount passes.

use std::sync::Arc;

use maho_cli::cli::kibitzer_budget::CliBudgetSlot;
use maho_cli::cli::kibitzer_tools::{
    create_kibitzer_host_tools, resolve_kibitzer_event_caps, resolve_kibitzer_tool_caps,
    KibitzerBudgetSource, KibitzerHostToolsInput,
};
use maho_ext_api::{AbortSignal, ToolCall, ToolContent, ToolDefinition, ToolResult};
use maho_omo_memory::kibitzer_contract::KibitzerBudgetSlot;
use maho_omo_memory::kibitzer_events::KibitzerEventCaps;
use maho_omo_memory::kibitzer_tools_caps::DEFAULT_KIBITZER_TOOL_CAPS;

fn call(params: serde_json::Value) -> ToolCall<'static> {
    ToolCall { id: "call-1", params, signal: AbortSignal::default(), on_update: None, context: None }
}

/// The mount passes the SESSION's slot; the host tools charge `slot.current()` per call.
fn tools(root: &std::path::Path, entries: Vec<serde_json::Value>, slot: Arc<CliBudgetSlot>) -> Vec<ToolDefinition> {
    let budget: KibitzerBudgetSource = Arc::new(move || slot.current());
    create_kibitzer_host_tools(KibitzerHostToolsInput {
        cwd: root.to_string_lossy().into_owned(),
        session_entries: Arc::new(move || entries.clone()),
        caps: DEFAULT_KIBITZER_TOOL_CAPS,
        budget,
    })
}

fn tool<'a>(tools: &'a [ToolDefinition], name: &str) -> &'a ToolDefinition {
    tools.iter().find(|tool| tool.name == name).expect("the tool is registered")
}

fn text(result: ToolResult) -> String {
    result.content.iter().map(|content| match content { ToolContent::Text { text, .. } => text.clone(), _ => String::new() }).collect()
}

#[test]
fn the_registry_is_the_three_cli_owned_names_in_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let names: Vec<String> = tools(dir.path(), Vec::new(), Arc::new(CliBudgetSlot::new(8))).iter().map(|tool| tool.name.clone()).collect();
    assert_eq!(names, ["read", "grep", "session_entries"]);
}

#[tokio::test]
async fn read_bounds_characters_and_redacts_secrets() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("note.txt"), format!("OPENAI_API_KEY=sk-live-abc\n{}", "x".repeat(7000))).expect("write");
    let tools = tools(dir.path(), Vec::new(), Arc::new(CliBudgetSlot::new(8)));
    let body = text((tool(&tools, "read").execute)(call(serde_json::json!({ "path": "note.txt" }))).await.expect("read ok"));
    assert!(body.contains("OPENAI_API_KEY=***"), "the secret is redacted");
    assert!(!body.contains("sk-live-abc"), "the credential is absent");
    let cap = DEFAULT_KIBITZER_TOOL_CAPS.read_chars;
    assert!(body.chars().count() <= cap + 64, "the body is bounded by the read cap: {} chars", body.chars().count());
}

#[tokio::test]
async fn read_rejects_a_path_outside_the_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tools = tools(dir.path(), Vec::new(), Arc::new(CliBudgetSlot::new(8)));
    let error = (tool(&tools, "read").execute)(call(serde_json::json!({ "path": "../escape.txt" }))).await.expect_err("escapes");
    assert!(error.to_string().contains("path_escape"), "{error}");
}

#[tokio::test]
async fn grep_matches_a_regular_expression_and_rejects_an_invalid_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "alpha\nbeta\ngamma\n").expect("write");
    let tools = tools(dir.path(), Vec::new(), Arc::new(CliBudgetSlot::new(8)));
    let body: serde_json::Value = serde_json::from_str(&text((tool(&tools, "grep").execute)(call(serde_json::json!({ "pattern": "^g.m+a$" }))).await.expect("grep ok"))).expect("json");
    let matches = body["matches"].as_array().expect("matches");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["line"], 3);
    let error = (tool(&tools, "grep").execute)(call(serde_json::json!({ "pattern": "(" }))).await.expect_err("invalid");
    assert!(error.to_string().contains("invalid_pattern"), "{error}");
}

#[tokio::test]
async fn session_entries_pages_by_cursor_and_hides_memory_channels() {
    let dir = tempfile::tempdir().expect("tempdir");
    let entries = vec![
        serde_json::json!({ "type": "message", "message": { "role": "user", "content": "hi" } }),
        serde_json::json!({ "type": "custom_message", "customType": "omo-kibitzer:gate", "content": "hidden" }),
        serde_json::json!({ "type": "message", "message": { "role": "assistant", "content": "hello" } }),
    ];
    let tools = tools(dir.path(), entries, Arc::new(CliBudgetSlot::new(8)));
    let page: serde_json::Value = serde_json::from_str(&text((tool(&tools, "session_entries").execute)(call(serde_json::json!({}))).await.expect("ok"))).expect("json");
    assert_eq!(page["hidden"], 1);
    assert_eq!(page["next_since"], 2);
    assert_eq!(page["entries"].as_array().expect("rows").len(), 2);
}

/// All host tools charge ONE session slot; two sessions' slots are independent.
#[tokio::test]
async fn all_host_tools_share_the_session_slot_and_sessions_are_isolated() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "x\n").expect("write");
    let session_a = Arc::new(CliBudgetSlot::new(2));
    let session_b = Arc::new(CliBudgetSlot::new(2));
    let tools = tools(dir.path(), Vec::new(), Arc::clone(&session_a));
    assert!((tool(&tools, "read").execute)(call(serde_json::json!({ "path": "a.txt" }))).await.is_ok(), "read charges 1");
    assert!((tool(&tools, "grep").execute)(call(serde_json::json!({ "pattern": "x" }))).await.is_ok(), "grep charges the same slot to 2");
    let error = (tool(&tools, "session_entries").execute)(call(serde_json::json!({}))).await.expect_err("shared slot exhausted");
    assert!(error.to_string().contains("tool_budget_exceeded"), "{error}");
    assert_eq!(session_a.current().used(), 2, "all host tools charged the one session slot");
    assert!(session_b.current().charge());
    assert_eq!(session_b.current().used(), 1);
    assert_eq!(session_a.current().used(), 2, "session A is unaffected by session B");
}

/// The sidecar resets the SAME slot per wake; the tools observe the reset budget.
#[tokio::test]
async fn a_per_wake_reset_changes_what_the_tools_observe() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "x\n").expect("write");
    let slot = Arc::new(CliBudgetSlot::new(3));
    let tools = tools(dir.path(), Vec::new(), Arc::clone(&slot));
    assert!((tool(&tools, "read").execute)(call(serde_json::json!({ "path": "a.txt" }))).await.is_ok());
    slot.reset(1);
    assert_eq!(slot.current().used(), 0, "the reset budget starts fresh");
    assert!((tool(&tools, "read").execute)(call(serde_json::json!({ "path": "a.txt" }))).await.is_ok(), "first charge of the new wake");
    let error = (tool(&tools, "grep").execute)(call(serde_json::json!({ "pattern": "x" }))).await.expect_err("new wake limit");
    assert!(error.to_string().contains("tool_budget_exceeded"), "{error}");
}

#[test]
fn an_empty_recall_block_yields_the_pinned_defaults() {
    let recall = serde_json::json!({});
    assert_eq!(resolve_kibitzer_tool_caps(&recall), DEFAULT_KIBITZER_TOOL_CAPS);
    assert_eq!(resolve_kibitzer_event_caps(&recall), KibitzerEventCaps::default());
}

#[test]
fn a_partial_tool_cap_override_preserves_the_other_defaults() {
    let recall = serde_json::json!({ "tool_caps": { "read_chars": 123 } });
    let caps = resolve_kibitzer_tool_caps(&recall);
    assert_eq!(caps.read_chars, 123);
    assert_eq!(caps.grep_matches, DEFAULT_KIBITZER_TOOL_CAPS.grep_matches);
    assert_eq!(caps.session_entries, DEFAULT_KIBITZER_TOOL_CAPS.session_entries);
    assert_eq!(caps.memory_read_chars, DEFAULT_KIBITZER_TOOL_CAPS.memory_read_chars);
}

#[test]
fn grep_scan_ms_is_preserved_as_i64_up_to_its_maximum() {
    let recall = serde_json::json!({ "tool_caps": { "grep_scan_ms": 2500 } });
    let scan_ms: i64 = resolve_kibitzer_tool_caps(&recall).grep_scan_ms;
    assert_eq!(scan_ms, 2500);
    let max = resolve_kibitzer_tool_caps(&serde_json::json!({ "tool_caps": { "grep_scan_ms": i64::MAX } }));
    assert_eq!(max.grep_scan_ms, i64::MAX, "the i64 field is preserved without truncation");
    assert_eq!(max.grep_matches, DEFAULT_KIBITZER_TOOL_CAPS.grep_matches, "an unrelated field is untouched");
}

#[test]
fn a_partial_event_cap_override_merges_per_field() {
    let recall = serde_json::json!({ "event_caps": { "prompt": 11 } });
    let caps = resolve_kibitzer_event_caps(&recall);
    assert_eq!(caps.prompt, 11);
    let default = KibitzerEventCaps::default();
    assert_eq!(caps.tool_args, default.tool_args);
    assert_eq!(caps.result_head, default.result_head);
    assert_eq!(caps.assistant, default.assistant);
}
