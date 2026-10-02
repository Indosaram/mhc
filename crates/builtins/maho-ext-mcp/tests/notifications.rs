use maho_ext_mcp::{logging::*,notification_schemas::*,notifications::*};
use serde_json::json;
#[test]
fn rfc_levels_map_to_logger_methods() {
    let mut log=McpServerLogging::new(None,None,0.0);
    for (level,method) in [("debug",LoggerMethod::Debug),("notice",LoggerMethod::Info),("warning",LoggerMethod::Warn),("critical",LoggerMethod::Error)]{assert_eq!(log.message(level,&json!("value"),Some("sub"),0.0),Some((method,"[server:sub] value".into())));}
}
#[test]
fn below_threshold_messages_are_filtered() {
    let mut log=McpServerLogging::new(Some("warning"),None,0.0);assert!(log.message("info",&json!("hidden"),None,0.0).is_none());assert!(log.message("error",&json!("kept"),None,0.0).is_some());
}
#[test]
fn flood_bucket_refills_on_exact_clock() {
    let mut log=McpServerLogging::new(None,None,0.0);let mut accepted=0;
    for _ in 0..100 {if log.message("info",&json!("burst"),None,0.0).is_some(){accepted+=1;}}
    assert_eq!(accepted,10);assert!(log.message("info",&json!("after"),None,1000.0).is_some());
}
#[test]
fn catalog_diff_is_sorted_and_deduplicated() {
    let strings=|items:&[&str]|items.iter().map(|s|(*s).to_owned()).collect::<Vec<_>>();
    let diff=diff_mcp_tool_names(&strings(&["a","b","c"]),&strings(&["b","d","e","e"]));assert_eq!(diff,McpCatalogDiff {added:strings(&["d","e"]),removed:strings(&["a","c"]),unchanged:strings(&["b"])});
}
#[test]
fn delta_reports_additions_as_inactive() {
    assert_eq!(format_mcp_list_changed_delta(&McpCatalogDiff {added:vec!["a".into()],removed:vec!["b".into()],unchanged:vec![]}),"1 added (inactive), 1 removed");assert_eq!(format_mcp_list_changed_delta(&McpCatalogDiff {added:vec![],removed:vec![],unchanged:vec![]}),"no change");
}
#[test]
fn notification_envelope_strips_unknown_keys_and_preserves_meta() {
    let parsed=parse_notification(&json!({"method":"notifications/tools/list_changed","unknown":true,"params":{"unknown":true,"_meta":{"x":1}}})).unwrap();
    assert_eq!(parsed,McpNotification::ListChanged {method:"notifications/tools/list_changed".into(),params:Some(json!({"_meta":{"x":1}}))});
}
#[test]
fn malformed_notifications_are_rejected() {
    for value in [json!({"method":"notifications/message","params":{"level":"warn"}}),json!({"method":"notifications/resources/updated","params":{"uri":4}}),json!({"method":"notifications/tools/list_changed","params":{"_meta":[]}})]{assert!(parse_notification(&value).is_none());}
}
#[tokio::test(start_paused=true)]
async fn burst_notifications_coalesce_into_one_refresh() {
    let coalescer=McpListChangeCoalescer::new(None,None);let (sender,mut receiver)=tokio::sync::mpsc::unbounded_channel();
    for _ in 0..10 {let sender=sender.clone();coalescer.notify(move||async move {sender.send(tokio::time::Instant::now()).unwrap();});}
    let started=tokio::time::Instant::now();let fired=receiver.recv().await.unwrap();assert_eq!(fired-started,std::time::Duration::from_millis(300));assert!(receiver.try_recv().is_err());
}
#[tokio::test(start_paused=true)]
async fn second_refresh_obeys_minimum_interval() {
    let coalescer=McpListChangeCoalescer::new(None,None);let (sender,mut receiver)=tokio::sync::mpsc::unbounded_channel();
    let first=sender.clone();coalescer.notify(move||async move {first.send(tokio::time::Instant::now()).unwrap();});let fired=receiver.recv().await.unwrap();
    coalescer.notify(move||async move {sender.send(tokio::time::Instant::now()).unwrap();});let second=receiver.recv().await.unwrap();assert_eq!(second-fired,std::time::Duration::from_secs(1));
}
#[tokio::test(start_paused=true)]
async fn refresh_errors_reach_sink_and_next_burst_still_runs() {
    use std::sync::Arc;
    let coalescer=McpListChangeCoalescer::new(None,None);
    let (sender,mut errors)=tokio::sync::mpsc::unbounded_channel();
    let sink=maho_ext_mcp::wrap::McpAsyncErrorSink {logger:Arc::new(move|_,data|{sender.send(data.clone()).unwrap();Ok(())}),notify:None};
    coalescer.notify_guarded(||async {Err(maho_ext_mcp::errors::McpError::new(maho_ext_mcp::errors::McpErrorKind::Protocol,"refresh failed"))},sink.clone());
    assert_eq!(errors.recv().await.unwrap()["message"],"refresh failed");
    coalescer.notify_guarded(||async {panic!("refresh panic");},sink);
    assert_eq!(errors.recv().await.unwrap()["message"],"refresh panic");
}
#[tokio::test(start_paused=true)]
async fn dispose_cancels_pending_refresh() {
    let coalescer=McpListChangeCoalescer::new(None,None);let (sender,receiver)=tokio::sync::oneshot::channel();
    coalescer.notify(move||async move {sender.send(()).unwrap();});coalescer.dispose();assert!(receiver.await.is_err());
}
#[tokio::test]
async fn removed_tool_definition_rejects_stale_execution() {
    let tool=build_mcp_tombstone_definition("mcp_fx_removed","fx");
    let result=(tool.execute)(maho_ext_api::ToolCall {id:"stale",params:json!({}),signal:Default::default(),on_update:None,context:None}).await;
    assert_eq!(result.unwrap_err().to_string(),"tool no longer available on fx");
}
#[tokio::test]
async fn subscriptions_drop_malformed_notifications_before_callbacks() {
    use std::sync::{Arc,Mutex};
    use maho_ext_mcp::{transport_sdk::{McpClient,McpTransportSpec},log::McpLogger};
    let root=tempfile::tempdir().unwrap();
    let client=McpClient::materialize("notifications",&McpTransportSpec::Http {url:"http://127.0.0.1:1/mcp".parse().unwrap(),headers:Default::default()},Arc::new(Mutex::new(McpLogger::new("notifications",root.path(),None).unwrap()))).await.unwrap();
    let (list_sender,mut lists)=tokio::sync::mpsc::unbounded_channel();
    let (resource_sender,mut resources)=tokio::sync::mpsc::unbounded_channel();
    let list_task=subscribe_mcp_list_changed(&client,Arc::new(move ||{list_sender.send(()).unwrap();}));
    let resource_task=maho_ext_mcp::resources::subscribe_mcp_resource_updated(&client,Arc::new(move ||{resource_sender.send(()).unwrap();}));
    client.notifications.send(json!({"method":"notifications/tools/list_changed","params":{"_meta":[]}})).unwrap();
    client.notifications.send(json!({"method":"notifications/resources/updated","params":{"uri":4}})).unwrap();
    client.notifications.send(json!({"method":"notifications/tools/list_changed"})).unwrap();
    client.notifications.send(json!({"method":"notifications/resources/updated","params":{"uri":"fixture://one"}})).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2),async {lists.recv().await.unwrap();resources.recv().await.unwrap();}).await.unwrap();
    assert!(lists.try_recv().is_err());assert!(resources.try_recv().is_err());
    list_task.abort();resource_task.abort();let _=list_task.await;let _=resource_task.await;
    client.close().await.unwrap();
}
