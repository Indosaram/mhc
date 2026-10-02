use maho_server::app_server::mcp_wire_status::{McpWireStatusAdapter,McpWireStatusRegistry};
use maho_ext_mcp::service_types::{McpWireAuthStatus,McpWireStatusServer,McpWireStatusSnapshot,McpWireTool};
use serde_json::json;
use std::{collections::BTreeMap,sync::{Arc,atomic::{AtomicUsize,Ordering}}};

fn snapshot(name: &str) -> McpWireStatusSnapshot {McpWireStatusSnapshot {servers:vec![McpWireStatusServer {name:name.into(),server_info:None,tools:vec![McpWireTool {name:"tool".into(),input_schema:json!({"type":"object"}),extra:BTreeMap::from([("description".into(),json!("description")),("private".into(),json!(true))])}],resources:Vec::new(),resource_templates:Vec::new(),auth_status:McpWireAuthStatus::Unsupported,status:None}]}}
#[test]
fn adapters_keep_global_and_thread_inventory_separate_and_release_subscriptions() {
    let disposed = Arc::new(AtomicUsize::new(0));let mut adapter = McpWireStatusAdapter::new(snapshot("thread"));
    let count = disposed.clone();adapter.bind_live_updates(move ||{count.fetch_add(1,Ordering::SeqCst);});
    adapter.update(snapshot("updated"));
    assert_eq!(adapter.server_statuses()[0]["tools"]["tool"]["description"],"description");
    assert!(adapter.server_statuses()[0]["tools"]["tool"].get("private").is_none());
    let mut registry = McpWireStatusRegistry::new(Some(McpWireStatusAdapter::new(snapshot("global"))));
    registry.register_thread("id".into(),adapter);
    assert_eq!(registry.resolve(None).unwrap().server_statuses()[0]["name"],"global");
    assert_eq!(registry.resolve(Some("id")).unwrap().server_statuses()[0]["name"],"updated");
    assert!(registry.resolve(Some("missing")).is_none());registry.remove_thread("id");
    assert_eq!(disposed.load(Ordering::SeqCst),1);
}
