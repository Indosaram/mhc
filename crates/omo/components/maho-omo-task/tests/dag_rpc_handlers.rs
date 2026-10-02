pub mod support;
use std::sync::{Arc,Mutex};
use maho_omo_task::dag_rpc_handlers::query_dag_rpc;
use senpi_task::dag::{manager::{DagManager, DagManagerOptions, DagStartParams, create_dag_manager}, store::{DagStoreConfig, DagStoreOptions, create_dag_file_store}};
use serde_json::{Value, json};
fn fixture() -> (tempfile::TempDir, DagManager, String) {
    let root = tempfile::tempdir().expect("temporary DAG directory");
    let store = create_dag_file_store(&DagStoreConfig::new(root.path()), DagStoreOptions::default()).expect("DAG store");
    let manager = create_dag_manager(DagManagerOptions { store: Arc::new(store), new_run_id: Some(Arc::new(|| "run-1".into())), now: Some(Arc::new(|| 1000)), materialize_skills: None, settings: None });
    let definition = senpi_task::dag::graph::DagDefinition { key: "key".into(), name: "run".into(), nodes: vec![senpi_task::dag::graph::DagNodeInput { id: "a".into(), prompt: "work".into(), target: senpi_task::dag::types::DagNodeTarget::Category("quick".into()), label: None, depends_on: None, task_summary: None, description: None, load_skills: None }] };
    let started = manager.start(DagStartParams { definition, parent_session_id: "parent".into(), root_session_id: "root".into() }).expect("DAG run start");
    (root, manager, started.snapshot.run_id)
}
fn query(manager: &DagManager, name: &str, value: Value) -> Value { query_dag_rpc(manager, Some("parent".into()), name, &value) }
#[tokio::test] async fn registered_query_handlers_observe_live_session_scope() {
    let (_root,manager,id)=fixture(); let mut api=support::api(); let session=Arc::new(Mutex::new(Some("parent".to_owned()))); let current=session.clone();
    maho_omo_task::dag_rpc_handlers::register_dag_rpc_handlers(&mut api,manager,Arc::new(move || current.lock().expect("session").clone())).expect("register");
    assert_eq!(api.registered.rpc_handlers.len(),4);
    for name in ["omo.dag.snapshot","omo.dag.history","omo.dag.subscribe"] { let result=(api.registered.rpc_handlers[name])(json!({"runId":id})).await.expect("request"); assert_eq!(result["ok"],true); }
    *session.lock().expect("session")=Some("foreign".into());
    let result=(api.registered.rpc_handlers["omo.dag.snapshot"])(json!({"runId":id})).await.expect("foreign"); assert_eq!(result["error"]["code"],"run_not_owned");
    *session.lock().expect("session")=None;
    let result=(api.registered.rpc_handlers["omo.dag.list"])(json!({})).await.expect("uncaptured"); assert_eq!(result["value"]["runs"],json!([]));
}
#[test] fn list_is_scoped_and_has_default_limit() { let (_root, manager, _) = fixture(); let value = query(&manager, "omo.dag.list", json!({})); assert_eq!(value["ok"], true); assert_eq!(value["value"]["limit"], 100); assert_eq!(value["value"]["runs"].as_array().unwrap().len(), 1); assert_eq!(query_dag_rpc(&manager, Some("other".into()), "omo.dag.list", &json!({}))["value"]["runs"], json!([])); }
#[test] fn list_filters_statuses() { let (_root, manager, _) = fixture(); assert_eq!(query(&manager, "omo.dag.list", json!({"statuses":["completed"]}))["value"]["runs"], json!([])); }
#[test] fn list_clamps_limit() { let (_root, manager, _) = fixture(); assert_eq!(query(&manager, "omo.dag.list", json!({"limit":999}))["value"]["limit"], 256); }
#[test] fn list_rejects_invalid_statuses() { let (_root, manager, _) = fixture(); for statuses in [json!("pending"), json!(["bad"]), json!([null])] { assert_eq!(query(&manager, "omo.dag.list", json!({"statuses":statuses}))["error"]["code"], "invalid_arguments"); } }
#[test] fn list_without_session_is_empty() { let (_root, manager, _) = fixture(); assert_eq!(query_dag_rpc(&manager, None, "omo.dag.list", &json!({}))["value"]["runs"], json!([])); }
#[test] fn snapshot_returns_owned_run() { let (_root, manager, id) = fixture(); assert_eq!(query(&manager, "omo.dag.snapshot", json!({"runId":id}))["value"]["runId"], id); }
#[test] fn snapshot_foreign_session_denied() { let (_root, manager, id) = fixture(); assert_eq!(query_dag_rpc(&manager, Some("other".into()), "omo.dag.snapshot", &json!({"runId":id}))["error"]["code"], "run_not_owned"); }
#[test] fn snapshot_without_session_denied() { let (_root, manager, id) = fixture(); assert_eq!(query_dag_rpc(&manager, None, "omo.dag.snapshot", &json!({"runId":id}))["error"]["code"], "run_not_owned"); }
#[test] fn all_run_scoped_queries_preserve_missing_and_foreign_errors() {
    let (_root,manager,id)=fixture();
    for method in ["omo.dag.snapshot","omo.dag.history","omo.dag.subscribe"] {
        assert_eq!(query_dag_rpc(&manager,Some("foreign".into()),method,&json!({"runId":id}))["error"]["code"],"run_not_owned");
        assert_eq!(query_dag_rpc(&manager,None,method,&json!({"runId":id}))["error"]["code"],"run_not_owned");
        assert_eq!(query(&manager,method,json!({"runId":"missing"}))["error"]["code"],"run_not_found");
    }
}
#[test] fn snapshot_missing_run_not_found() { let (_root, manager, _) = fixture(); assert_eq!(query(&manager, "omo.dag.snapshot", json!({"runId":"missing"}))["error"]["code"], "run_not_found"); }
#[test] fn history_reads_real_creation_journal() { let (_root, manager, id) = fixture(); let result = query(&manager, "omo.dag.history", json!({"runId":id})); assert_eq!(result["ok"], true); assert_eq!(result["value"]["events"][0]["type"], "dag.run.created"); assert_eq!(result["value"]["events"][0]["seq"], 1); }
#[test] fn history_excludes_prior_sequences() { let (_root, manager, id) = fixture(); assert_eq!(query(&manager, "omo.dag.history", json!({"runId":id,"sinceSeq":1}))["value"]["events"], json!([])); }
#[test] fn history_unknown_event_types_are_valid_empty_filter() { let (_root, manager, id) = fixture(); assert_eq!(query(&manager, "omo.dag.history", json!({"runId":id,"types":["future.event"]}))["value"]["events"], json!([])); }
#[test] fn history_known_event_filter_preserves_event() { let (_root, manager, id) = fixture(); assert_eq!(query(&manager, "omo.dag.history", json!({"runId":id,"types":["dag.run.created","future.event"]}))["value"]["events"].as_array().unwrap().len(), 1); }
#[test] fn subscribe_freezes_history_at_snapshot_watermark() { let (_root, manager, id) = fixture(); let result = query(&manager, "omo.dag.subscribe", json!({"runId":id})); assert_eq!(result["ok"], true); assert_eq!(result["value"]["eventName"], "omo.dag.event"); assert_eq!(result["value"]["highWaterSeq"], result["value"]["snapshot"]["lastSeq"]); }
#[test] fn subscribe_honors_lower_through_sequence() { let (_root, manager, id) = fixture(); assert_eq!(query(&manager, "omo.dag.subscribe", json!({"runId":id,"throughSeq":0}))["value"]["page"]["events"], json!([])); }
#[test] fn all_queries_reject_nonobjects() { let (_root, manager, _) = fixture(); for name in ["omo.dag.list","omo.dag.snapshot","omo.dag.history","omo.dag.subscribe"] { for value in [json!(null), json!([]), json!("bad")] { assert_eq!(query(&manager, name, value)["error"]["code"], "invalid_arguments"); } } }
#[test] fn invalid_limits_rejected() { let (_root, manager, id) = fixture(); for limit in [json!(0), json!(-1), json!(1.5), json!("1"), json!(null)] { assert_eq!(query(&manager, "omo.dag.history", json!({"runId":id,"limit":limit}))["error"]["code"], "invalid_arguments"); } }
#[test] fn invalid_sequences_rejected() { let (_root, manager, id) = fixture(); for field in ["sinceSeq", "throughSeq"] { for invalid in [json!(-1), json!(1.5), json!("1"), json!(null)] { let mut value = json!({"runId":id}); value[field] = invalid; assert_eq!(query(&manager, "omo.dag.history", value)["error"]["code"], "invalid_arguments"); } } }
#[test] fn invalid_lanes_rejected() { let (_root, manager, id) = fixture(); for lane in [json!("other"), json!(null), json!(1)] { assert_eq!(query(&manager, "omo.dag.history", json!({"runId":id,"lane":lane}))["error"]["code"], "invalid_arguments"); } }
#[test] fn invalid_event_types_rejected() { let (_root, manager, id) = fixture(); for types in [json!("bad"), json!([""]), json!([" "]), json!([null])] { assert_eq!(query(&manager, "omo.dag.history", json!({"runId":id,"types":types}))["error"]["code"], "invalid_arguments"); } }
#[test] fn invalid_run_identifiers_rejected() { let (_root, manager, _) = fixture(); for id in [json!(null), json!(""), json!(" "), json!(1)] { assert_eq!(query(&manager, "omo.dag.snapshot", json!({"runId":id}))["error"]["code"], "invalid_arguments"); } }
#[test] fn bounded_subscribe_page_walk_does_not_shift_when_journal_grows() {
    use senpi_task::dag::{journal::{create_dag_journal,DagJournalOptions},types::DagRunEventPayload};
    let (root,manager,id)=fixture(); let store=Arc::new(create_dag_file_store(&DagStoreConfig::new(root.path()),DagStoreOptions::default()).expect("store")); let record=manager.record(&id,"parent").expect("record"); let journal=create_dag_journal(DagJournalOptions { store,run_id:id.clone(),initial_checkpoint:record,apply_event:Arc::new(|record,_| record.clone()),subscriber_ring:None,now:Some(Arc::new(|| 1000)) }).expect("journal");
    for _ in 0..4 { journal.append(DagRunEventPayload::RunStarted { generation:1 }).expect("append"); }
    let handshake=query(&manager,"omo.dag.subscribe",json!({"runId":id,"limit":2})); assert_eq!(handshake["ok"],true); assert_eq!(handshake["value"]["highWaterSeq"],5); let mut page=handshake["value"]["page"].clone(); let mut sequences=Vec::new();
    loop { sequences.extend(page["events"].as_array().expect("events").iter().map(|event| event["seq"].as_u64().expect("seq"))); if page["hasMore"]!=true { break; } journal.append(DagRunEventPayload::RunStarted { generation:1 }).expect("append while paging"); let response=query(&manager,"omo.dag.history",json!({"runId":id,"sinceSeq":page["nextSinceSeq"],"throughSeq":5,"limit":2})); assert_eq!(response["ok"],true); page=response["value"].clone(); }
    assert_eq!(sequences,[1,2,3,4,5]); assert!(query(&manager,"omo.dag.history",json!({"runId":id}))["value"]["headSeq"].as_u64().expect("head")>5);
}
#[test] fn unreadable_journal_returns_history_unavailable_without_losing_snapshot() {
    let (root,manager,id)=fixture(); let store=create_dag_file_store(&DagStoreConfig::new(root.path()),DagStoreOptions::default()).expect("store"); let path=store.paths.event(&id);
    std::fs::rename(&path,path.with_extension("saved")).expect("preserve journal"); std::fs::create_dir(&path).expect("unreadable journal directory");
    assert_eq!(query(&manager,"omo.dag.snapshot",json!({"runId":id}))["ok"],true);
    for method in ["omo.dag.history","omo.dag.subscribe"] { let response=query(&manager,method,json!({"runId":id})); assert_eq!(response["ok"],false); assert_eq!(response["error"]["code"],"history_unavailable"); }
}
