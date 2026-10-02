use maho_ext_mcp::{service_snapshot::build_mcp_server_snapshot,service_types::*};

#[test]
fn removed_server_has_no_connection_or_counters() {
    let snapshot=build_mcp_server_snapshot("removed",None,None,None,1000.0);
    let wire=serde_json::to_value(snapshot).unwrap();
    assert_eq!(wire["configState"],"removed");
    assert_eq!(wire["lifecycleState"],"not_spawned");
    assert!(wire["generation"].is_null());
    assert!(wire["uptimeMs"].is_null());
    assert_eq!(wire["counters"],serde_json::to_value(McpServerCounters::default()).unwrap());
}

#[tokio::test]
async fn status_collection_preserves_input_order_under_concurrent_completion() {
    use maho_ext_mcp::{expose::status::McpServerExposureStatus,status::build_mcp_status_rows};
    let rows=build_mcp_status_rows(vec![build_mcp_server_snapshot("a",None,None,None,0.0),build_mcp_server_snapshot("b",None,None,None,0.0)],|_|async {McpServerExposureStatus {hint:None,tool_count:Some(2),mode:None}}).await;
    assert_eq!(rows.iter().map(|row|row.snapshot.name.as_str()).collect::<Vec<_>>(),vec!["a","b"]);
    assert!(rows.iter().all(|row|row.exposure.tool_count==Some(2)));
}
