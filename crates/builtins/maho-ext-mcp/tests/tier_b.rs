use std::{collections::BTreeSet, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use maho_ext_api::{AbortSignal, ToolCall, ToolDefinition, ToolResult};
use maho_ext_mcp::expose::tier_b::*;
use serde_json::json;

#[test]
fn active_order_preserves_base_and_sorts_catalog_across_promotions() {
    let catalog = BTreeSet::from(["weather_forecast".into(), "calendar_create".into()]);
    let reference = vec!["base_first".into(), "base_second".into()];
    let first = order_active_set(&["base_second".into(), "weather_forecast".into(), "base_first".into()], &reference, &catalog);
    let mut next = first.clone(); next.extend(["calendar_create".into(), "weather_forecast".into()]);
    assert_eq!(order_active_set(&next, &first, &catalog), ["base_first", "base_second", "calendar_create", "weather_forecast"]);
}
#[tokio::test]
async fn stub_promotes_before_executing_full_definition_with_original_arguments() {
    let count = Arc::new(AtomicUsize::new(0)); let execute_count = count.clone();
    let full = ToolDefinition::new("mcp_fx_tool", "full", json!({"type":"object"}), Arc::new(move |call| {
        assert_eq!(execute_count.load(Ordering::SeqCst), 1);
        Box::pin(async move {call.signal.check()?; Ok(ToolResult::text(call.params["value"].as_str().unwrap()))})
    }));
    let promote_count = count.clone();
    let stub = build_mcp_stub_definition("mcp_fx_tool", Some(McpStubPromotion {full, promote: Arc::new(move || {promote_count.fetch_add(1, Ordering::SeqCst);Ok(())})}));
    assert_eq!(stub.parameters["additionalProperties"], true);
    let result = (stub.execute)(ToolCall {id:"call", params:json!({"value":"first"}), signal:AbortSignal::default(), on_update:None, context:None}).await.unwrap();
    assert_eq!(result, ToolResult::text("first")); assert_eq!(count.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn unpromotable_stub_returns_guidance_without_details() {
    let stub = build_mcp_stub_definition("mcp_fx_tool", None);
    let result = (stub.execute)(ToolCall {id:"call", params:json!({}), signal:AbortSignal::default(), on_update:None, context:None}).await.unwrap();
    assert!(result.details.is_none()); assert_eq!(result.content.len(), 1);
}

#[tokio::test]
async fn failed_stub_promotion_does_not_execute_full_tool() {
    let count=Arc::new(AtomicUsize::new(0));let observed=count.clone();
    let full=ToolDefinition::new("mcp_fx_tool","full",json!({"type":"object"}),Arc::new(move |_| {observed.fetch_add(1,Ordering::SeqCst);Box::pin(async {Ok(ToolResult::text("ok"))})}));
    let stub=build_mcp_stub_definition("mcp_fx_tool",Some(McpStubPromotion {full,promote:Arc::new(||Err(maho_ext_api::ExtensionFailure::new("publication rejected")))}));
    let result=(stub.execute)(ToolCall {id:"call",params:json!({}),signal:AbortSignal::default(),on_update:None,context:None}).await;
    assert!(result.is_err());assert_eq!(count.load(Ordering::SeqCst),0);
}
