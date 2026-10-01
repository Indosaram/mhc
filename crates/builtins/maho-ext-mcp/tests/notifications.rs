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
async fn dispose_cancels_pending_refresh() {
    let coalescer=McpListChangeCoalescer::new(None,None);let (sender,receiver)=tokio::sync::oneshot::channel();
    coalescer.notify(move||async move {sender.send(()).unwrap();});coalescer.dispose();assert!(receiver.await.is_err());
}
