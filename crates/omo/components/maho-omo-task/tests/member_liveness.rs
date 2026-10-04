use maho_omo_task::member_liveness::{liveness_details,liveness_delivery_keys,liveness_delivery_keys_from_session_text};
use senpi_task::state::{TaskRecordInput,TaskStatus,ResidencyState,create_task_record};
use serde_json::json;
fn record() -> senpi_task::state::TaskRecord { let mut record = create_task_record(TaskRecordInput { name: Some("team:11111111-1111-4111-8111-111111111111:alpha".into()), ..Default::default() },Some(1)).expect("member task record"); record.status = TaskStatus::Error; record }
#[test] fn abnormal_member_has_structured_liveness_details() { let mut record = record(); record.error_message = Some("crashed".into()); record.killed = Some(true); let details = liveness_details(&record).unwrap(); assert_eq!(details["memberName"],"alpha"); assert_eq!(details["lastKnownState"],"error"); assert_eq!(details["reason"],"crashed"); assert_eq!(details["killed"],true); }
#[test] fn suspended_members_never_report_death() { for residency in [ResidencyState::PersistedOnly,ResidencyState::RpcDetached] { let mut record = record(); record.residency_state = residency; assert!(liveness_details(&record).is_none()); } }
#[test] fn healthy_member_has_no_liveness_notification() { let mut record = record(); record.status = TaskStatus::Completed; assert!(liveness_details(&record).is_none()); }
#[test] fn completed_killed_resident_preserves_last_completed_state() { let mut record=record(); record.status=TaskStatus::Completed; record.residency_state=ResidencyState::Disposed; record.killed=Some(true); record.error_message=Some("reattach disabled for crashed resident".into()); let details=liveness_details(&record).expect("killed resident"); assert_eq!(details["lastKnownState"],"completed"); assert_eq!(details["reason"],record.error_message.expect("reason")); assert_eq!(details["killed"],true); }
#[test] fn ordinary_task_has_no_member_liveness() { let mut record = record(); record.name = Some("worker".into()); assert!(liveness_details(&record).is_none()); }
#[test] fn killed_cancelled_member_reports_death() { let mut record = record(); record.status = TaskStatus::Cancelled; record.killed = Some(true); assert!(liveness_details(&record).is_some()); }
#[test] fn direct_message_extracts_exact_delivery_key() { assert_eq!(liveness_delivery_keys(&json!({"message":{"customType":"senpi-task.team-member-liveness","details":{"deliveryKey":"team-member-liveness:st_1:0"}}})),["team-member-liveness:st_1:0"]); }
#[test] fn wake_envelope_extracts_and_deduplicates_keys() { let detail = json!({"customType":"senpi-task.team-member-liveness","details":{"deliveryKey":"team-member-liveness:st_1:0"}}); assert_eq!(liveness_delivery_keys(&json!({"message":{"customType":"omo-senpi:wake","details":[detail,detail]}})),["team-member-liveness:st_1:0"]); }
#[test] fn session_marker_parser_ignores_invalid_json_and_other_entries() { let message = json!({"type":"custom_message","customType":"senpi-task.team-member-liveness","details":{"deliveryKey":"team-member-liveness:st_1:0"}}); let text = format!("bad json\n{}\n{{\"type\":\"message\"}}\n",message); assert_eq!(liveness_delivery_keys_from_session_text(&text),["team-member-liveness:st_1:0"]); }
#[test] fn foreign_key_prefix_is_not_acknowledged() { assert!(liveness_delivery_keys(&json!({"message":{"customType":"senpi-task.team-member-liveness","details":{"deliveryKey":"other:st_1"}}})).is_empty()); }

use std::sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}};
use maho_omo_task::{status_ui::StatusUiTimers,member_liveness::{create_team_member_liveness_notifier,TeamMemberLivenessDeps}};
type TimerCallback=Box<dyn FnOnce()+Send>;
#[derive(Default)] struct Timers(Mutex<Vec<TimerCallback>>);
impl StatusUiTimers for Timers { fn set(&self,callback:TimerCallback,_:u64)->u64 { let mut callbacks=self.0.lock().expect("callbacks"); callbacks.push(callback); callbacks.len() as u64 } fn clear(&self,_:u64) {} }
impl Timers { fn fire(&self) { let callback=self.0.lock().expect("callbacks").remove(0); callback(); } }
#[test] fn pending_delivery_is_deduplicated_by_task_epoch() {
    let calls=Arc::new(AtomicUsize::new(0)); let delivered=calls.clone(); let notifier=create_team_member_liveness_notifier(TeamMemberLivenessDeps { deliver:Arc::new(move |_,_,_| { delivered.fetch_add(1,Ordering::SeqCst); Ok(()) }),was_delivered:Arc::new(|_| false),mark_delivered:Arc::new(|_| panic!("not persisted")),on_error:Arc::new(|error| panic!("{error}")),timers:Arc::new(Timers::default()),max_delivery_retries:None,max_persistence_retries:None });
    let mut record=record(); notifier.notify_terminal(&record); notifier.notify_terminal(&record); assert_eq!(calls.load(Ordering::SeqCst),1); record.notification.run_epoch+=1; notifier.notify_terminal(&record); assert_eq!(calls.load(Ordering::SeqCst),2);
}
#[test] fn persisted_epoch_suppresses_redelivery() {
    let notifier=create_team_member_liveness_notifier(TeamMemberLivenessDeps { deliver:Arc::new(|_,_,_| panic!("already persisted")),was_delivered:Arc::new(|_| true),mark_delivered:Arc::new(|_| {}),on_error:Arc::new(|error| panic!("{error}")),timers:Arc::new(Timers::default()),max_delivery_retries:None,max_persistence_retries:None }); notifier.notify_terminal(&record());
}
#[test] fn failed_delivery_retries_exactly_to_configured_limit() {
    let calls=Arc::new(AtomicUsize::new(0)); let delivered=calls.clone(); let timers=Arc::new(Timers::default()); let notifier=create_team_member_liveness_notifier(TeamMemberLivenessDeps { deliver:Arc::new(move |_,_,_| { delivered.fetch_add(1,Ordering::SeqCst); Err("failure".into()) }),was_delivered:Arc::new(|_| false),mark_delivered:Arc::new(|_| {}),on_error:Arc::new(|_| {}),timers:timers.clone(),max_delivery_retries:Some(2),max_persistence_retries:None }); notifier.notify_terminal(&record()); timers.fire(); timers.fire(); assert_eq!(calls.load(Ordering::SeqCst),3); assert!(timers.0.lock().expect("callbacks").is_empty());
}
#[test] fn persistence_ack_waits_for_exact_jsonl_marker_and_clears_pending_epoch() {
    let captured=Arc::new(Mutex::new(None)); let session=captured.clone(); let persisted=Arc::new(Mutex::new(Vec::new())); let sink=persisted.clone(); let timers=Arc::new(Timers::default()); let notifier=create_team_member_liveness_notifier(TeamMemberLivenessDeps { deliver:Arc::new(|_,_,_| Ok(())),was_delivered:Arc::new(|_| false),mark_delivered:Arc::new(move |record| sink.lock().expect("persisted").push(record.notification.run_epoch)),on_error:Arc::new(|error| panic!("{error}")),timers:timers.clone(),max_delivery_retries:None,max_persistence_retries:Some(1) });
    let mut record=record(); record.task_id="st_00000001".into(); notifier.notify_terminal(&record); notifier.acknowledge_persisted(Arc::new(move || session.lock().expect("session").clone())); assert!(persisted.lock().expect("persisted").is_empty()); assert_eq!(timers.0.lock().expect("timers").len(),1);
    *captured.lock().expect("session")=Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/liveness-session.jsonl")); timers.fire(); assert_eq!(*persisted.lock().expect("persisted"),vec![0]); assert!(timers.0.lock().expect("timers").is_empty());
}
#[test] fn owned_resident_error_notifies_but_suspended_and_foreign_members_do_not() {
    use senpi_task::{store::StateDirConfig,team::{runtime_config::{TeamTaskBounds,to_team_core_config},storage::team_storage_base_dir,normalize::normalize_senpi_team_spec,liveness_ownership::TeamMemberOwnershipDeps}};
    use team_core::{team_state_store::create_runtime_state,types::SpecSource};
    let root=tempfile::tempdir().expect("root"); let state_dir=StateDirConfig { project_dir:root.path().into(),task_state_dir:None }; let bounds=TeamTaskBounds { max_members:4,max_parallel_members:2,max_wall_clock_minutes:10 }; let config=to_team_core_config(&bounds,&team_storage_base_dir(&state_dir).to_string_lossy()).expect("config"); let spec=normalize_senpi_team_spec(&json!({"members":[{"name":"alpha","kind":"category","category":"quick","prompt":"work"}]}),"squad",None).expect("spec"); let state=create_runtime_state(&spec,Some("lead"),SpecSource::Project,&config).expect("state");
    let calls=Arc::new(AtomicUsize::new(0)); let sink=calls.clone(); let notifier=create_team_member_liveness_notifier(TeamMemberLivenessDeps { deliver:Arc::new(move |_,_,_| { sink.fetch_add(1,Ordering::SeqCst); Ok(()) }),was_delivered:Arc::new(|_| false),mark_delivered:Arc::new(|_| {}),on_error:Arc::new(|error| panic!("{error}")),timers:Arc::new(Timers::default()),max_delivery_retries:None,max_persistence_retries:None }); let current=Arc::new(Mutex::new(Some("lead".into()))); let session=current.clone();
    let observe=maho_omo_task::owned_member_liveness::create_owned_member_liveness_notifier(TeamMemberOwnershipDeps { state_dir,team_bounds:bounds,load_runtime_state:None },Arc::new(move || session.lock().expect("session").clone()),notifier); let mut member=record(); member.name=Some(format!("team:{}:alpha",state.team_run_id));
    for residency in [ResidencyState::PersistedOnly,ResidencyState::RpcDetached] { member.residency_state=residency; observe(&member); } assert_eq!(calls.load(Ordering::SeqCst),0); member.residency_state=ResidencyState::Resident; *current.lock().expect("session")=Some("foreign".into()); observe(&member); assert_eq!(calls.load(Ordering::SeqCst),0); *current.lock().expect("session")=Some("lead".into()); observe(&member); assert_eq!(calls.load(Ordering::SeqCst),1);
}
#[test] fn durable_marker_binding_acknowledges_only_matching_status_and_epoch() {
    use senpi_task::store::{StateDirConfig,TaskRecordStore};
    let root=tempfile::tempdir().expect("root"); let store=TaskRecordStore::new(&StateDirConfig { project_dir:root.path().into(),task_state_dir:None }); let mut record=record(); store.save(&record).expect("save"); let deps=maho_omo_task::member_liveness::bind_liveness_store_markers(TeamMemberLivenessDeps { deliver:Arc::new(|_,_,_| Ok(())),was_delivered:Arc::new(|_| panic!("replaced")),mark_delivered:Arc::new(|_| panic!("replaced")),on_error:Arc::new(|error| panic!("{error}")),timers:Arc::new(Timers::default()),max_delivery_retries:None,max_persistence_retries:None },store.clone());
    assert!(!(deps.was_delivered)(&record)); let captured=record.clone(); record.notification.run_epoch=1; store.replace(&record).expect("next epoch"); (deps.mark_delivered)(&captured); assert!(store.load(&record.task_id).expect("load").expect("record").notification.liveness_notified_epoch.is_none());
    record.status=TaskStatus::Completed; store.replace(&record).expect("status"); (deps.mark_delivered)(&senpi_task::state::TaskRecord { status:TaskStatus::Error,..record.clone() }); assert!(store.load(&record.task_id).expect("load").expect("record").notification.liveness_notified_epoch.is_none());
    (deps.mark_delivered)(&record); assert!((deps.was_delivered)(&record)); assert_eq!(store.load(&record.task_id).expect("load").expect("record").notification.liveness_notified_epoch,Some(1));
}

#[test]
fn deferred_liveness_rejection_retries_once_without_marking_delivery() {
    let callbacks = Arc::new(Mutex::new(Vec::<senpi_task::completion::DeliveryCallbacks>::new()));
    let captured = callbacks.clone();
    let timers = Arc::new(Timers::default());
    let errors = Arc::new(AtomicUsize::new(0));
    let rejected = errors.clone();
    let notifier = create_team_member_liveness_notifier(TeamMemberLivenessDeps {
        deliver: Arc::new(move |_, _, callback| { captured.lock().expect("callbacks").push(callback); Ok(()) }),
        was_delivered: Arc::new(|_| false), mark_delivered: Arc::new(|_| panic!("no persisted marker")),
        on_error: Arc::new(move |_| { rejected.fetch_add(1, Ordering::SeqCst); }),
        timers: timers.clone(), max_delivery_retries: Some(1), max_persistence_retries: None,
    });
    notifier.notify_terminal(&record());
    let first = callbacks.lock().expect("callbacks")[0].clone();
    first.failed(senpi_task::host::HostError { message: "rejected wake".into() });
    first.failed(senpi_task::host::HostError { message: "duplicate".into() });
    first.delivered();
    assert_eq!(errors.load(Ordering::SeqCst), 1);
    assert_eq!(timers.0.lock().expect("timers").len(), 1);
    timers.fire();
    assert_eq!(callbacks.lock().expect("callbacks").len(), 2);
    let second = callbacks.lock().expect("callbacks")[1].clone();
    second.delivered();
    notifier.notify_terminal(&record());
    assert_eq!(callbacks.lock().expect("callbacks").len(), 2);
}
