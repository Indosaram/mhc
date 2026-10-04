use maho_ext_mcp::index::publish_wire_status;
use maho_ext_mcp::service_types::{McpWireStatusServer, McpWireStatusSnapshot, McpWireAuthStatus};
use std::sync::{Arc, Mutex};

#[test]
fn publish_wire_status_reaches_a_native_subscriber_on_the_pinned_channel() {
    let events = maho_ext_api::EventBus::default();
    let observed = Arc::new(Mutex::new(Vec::<McpWireStatusSnapshot>::new()));
    let sink = observed.clone();
    let _subscription = events.on_native::<McpWireStatusSnapshot>(
        maho_core::agent_session::MCP_WIRE_STATUS_CHANGED_EVENT,
        Arc::new(move |snapshot| sink.lock().expect("observed").push(snapshot.clone())),
    );
    let snapshot = McpWireStatusSnapshot { servers: vec![McpWireStatusServer { name: "server".into(), server_info: None, tools: Vec::new(), resources: Vec::new(), resource_templates: Vec::new(), auth_status: McpWireAuthStatus::Unsupported, status: None }] };
    publish_wire_status(&events, &snapshot);
    let observed = observed.lock().expect("observed");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].servers[0].name, "server");
}

#[test]
fn publish_wire_status_is_a_noop_without_subscribers() {
    let events = maho_ext_api::EventBus::default();
    publish_wire_status(&events, &McpWireStatusSnapshot::default());
}
