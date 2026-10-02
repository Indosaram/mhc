use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use maho_omo_task::{dag_rpc_bridge::create_dag_rpc_bridge, dag_rpc_bridge_contract::{DagBridgeRun, DagRpcBridgeDeps}, status_ui::StatusUiTimers};
use serde_json::{Value, json};
type Callback = Box<dyn FnOnce() + Send>;
#[derive(Default)] struct Timers { next: Mutex<u64>, callbacks: Mutex<BTreeMap<u64,(u64,Callback)>> }
impl StatusUiTimers for Timers {
    fn set(&self, callback: Callback, ms: u64) -> u64 { let mut next = self.next.lock().expect("timer counter lock"); *next += 1; self.callbacks.lock().expect("timer callbacks lock").insert(*next,(ms,callback)); *next }
    fn clear(&self, handle: u64) { self.callbacks.lock().expect("timer callbacks lock").remove(&handle); }
}
impl Timers { fn fire(&self, ms: u64) { let id = self.callbacks.lock().expect("timer callbacks lock").iter().find(|(_, (delay,_))| *delay == ms).map(|(id,_)| *id); if let Some(id) = id { let (_,callback) = self.callbacks.lock().expect("timer callbacks lock").remove(&id).expect("registered timer"); callback(); } } fn count(&self) -> usize { self.callbacks.lock().expect("timer callbacks lock").len() } }
type Events = Arc<Mutex<Vec<(String, Value)>>>;
fn fixture() -> (Arc<maho_omo_task::dag_rpc_bridge::DagRpcBridge>, Arc<Timers>, Events) {
    let timers = Arc::new(Timers::default()); let events: Events = Arc::new(Mutex::new(Vec::new())); let sink = events.clone();
    let bridge = create_dag_rpc_bridge(DagRpcBridgeDeps { live_runs: Arc::new(|| vec![DagBridgeRun { run_id: "run".into(), status: "running".into(), subscribe: Arc::new(|_| Box::new(|| {})) }]), run_snapshots: Some(Arc::new(Vec::new)), parent_session_id: Arc::new(|| Some("parent".into())), emit: Arc::new(move |name,value| sink.lock().expect("events lock").push((name.into(),value))), timers: timers.clone(), now: Arc::new(|| 0), heartbeat_ms: None, activity_coalesce_ms: None, snapshot_debounce_ms: None });
    (bridge,timers,events)
}
#[test] fn ledger_dedupes_sequences_per_run() { let (bridge,_,events) = fixture(); bridge.attach(); bridge.forward(&json!({"runId":"run","seq":1})); bridge.forward(&json!({"runId":"run","seq":1})); bridge.forward(&json!({"runId":"run","seq":0})); assert_eq!(events.lock().unwrap().len(), 1); }
#[test] fn configured_heartbeat_stops_after_live_run_becomes_terminal() {
    let timers=Arc::new(Timers::default()); let events:Events=Arc::new(Mutex::new(vec![])); let sink=events.clone(); let status=Arc::new(Mutex::new("running".to_owned())); let current=status.clone();
    let bridge=create_dag_rpc_bridge(DagRpcBridgeDeps { live_runs:Arc::new(move || vec![DagBridgeRun { run_id:"run".into(),status:current.lock().expect("status").clone(),subscribe:Arc::new(|_| Box::new(|| {})) }]),run_snapshots:None,parent_session_id:Arc::new(|| Some("parent".into())),emit:Arc::new(move |name,value| sink.lock().expect("events").push((name.into(),value))),timers:timers.clone(),now:Arc::new(|| 0),heartbeat_ms:Some(123),activity_coalesce_ms:None,snapshot_debounce_ms:None });
    bridge.attach(); timers.fire(50); timers.fire(123); assert_eq!(events.lock().expect("events").len(),1);
    *status.lock().expect("status")="completed".into(); timers.fire(123); assert_eq!(events.lock().expect("events").len(),1); assert_eq!(timers.count(),0);
    bridge.detach(); bridge.attach(); timers.fire(50); assert_eq!(timers.count(),0); assert_eq!(events.lock().expect("events").len(),1); bridge.dispose();
}
#[test] fn telemetry_coalesces_latest_per_node() { let (bridge,timers,events) = fixture(); bridge.attach(); bridge.publish_activity(json!({"runId":"run","nodeId":"a","activity":"first"})); bridge.publish_activity(json!({"runId":"run","nodeId":"a","activity":"last"})); timers.fire(150); let events = events.lock().unwrap(); assert_eq!(events.len(), 1); assert_eq!(events[0].0, "omo.dag.activity"); assert_eq!(events[0].1["activity"], "last"); }
#[test] fn snapshot_fingerprint_suppresses_duplicate_flush() { let (bridge,timers,events) = fixture(); bridge.attach(); timers.fire(50); bridge.notify_store_mutation(); timers.fire(50); assert_eq!(events.lock().unwrap().len(), 1); }
#[test] fn heartbeat_reports_last_delivered_sequence() { let (bridge,timers,events) = fixture(); bridge.attach(); bridge.forward(&json!({"runId":"run","seq":4})); timers.fire(15000); let events = events.lock().unwrap(); assert_eq!(events[1].0,"omo.dag.heartbeat"); assert_eq!(events[1].1["runs"][0]["headSeq"],4); assert_eq!(events[1].1["at"],"1970-01-01T00:00:00.000Z"); }
#[test] fn detach_clears_all_timers_and_pending_activity() { let (bridge,timers,events) = fixture(); bridge.attach(); bridge.publish_activity(json!({"runId":"run","nodeId":"a"})); bridge.detach(); assert_eq!(timers.count(),0); timers.fire(150); bridge.forward(&json!({"runId":"run","seq":1})); assert!(events.lock().unwrap().is_empty()); }
#[test] fn reattach_resets_snapshot_fingerprint() { let (bridge,timers,events) = fixture(); bridge.attach(); timers.fire(50); bridge.detach(); bridge.attach(); timers.fire(50); assert_eq!(events.lock().unwrap().len(),2); }
#[test] fn disposal_prevents_future_attachment() { let (bridge,timers,_) = fixture(); bridge.attach(); bridge.dispose(); bridge.attach(); assert_eq!(timers.count(),0); }
#[test] fn activity_preserves_first_insertion_order_when_updated() {
    let (bridge,timers,events) = fixture(); bridge.attach();
    for (node,activity) in [("z","first"),("a","second"),("z","last")] { bridge.publish_activity(json!({"runId":"run","nodeId":node,"activity":activity})); }
    timers.fire(150); let events = events.lock().expect("events lock");
    assert_eq!(events.iter().map(|(_,value)| value["nodeId"].as_str().expect("node")).collect::<Vec<_>>(),["z","a"]);
    assert_eq!(events[0].1["activity"],"last");
}
