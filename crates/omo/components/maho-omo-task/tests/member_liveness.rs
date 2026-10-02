use maho_omo_task::member_liveness::{liveness_details,liveness_delivery_keys,liveness_delivery_keys_from_session_text};
use senpi_task::state::{TaskRecordInput,TaskStatus,ResidencyState,create_task_record};
use serde_json::json;
fn record() -> senpi_task::state::TaskRecord { let mut record = create_task_record(TaskRecordInput { name: Some("team:11111111-1111-4111-8111-111111111111:alpha".into()), ..Default::default() },Some(1)).expect("member task record"); record.status = TaskStatus::Error; record }
#[test] fn abnormal_member_has_structured_liveness_details() { let mut record = record(); record.error_message = Some("crashed".into()); record.killed = Some(true); let details = liveness_details(&record).unwrap(); assert_eq!(details["memberName"],"alpha"); assert_eq!(details["lastKnownState"],"error"); assert_eq!(details["reason"],"crashed"); assert_eq!(details["killed"],true); }
#[test] fn suspended_members_never_report_death() { for residency in [ResidencyState::PersistedOnly,ResidencyState::RpcDetached] { let mut record = record(); record.residency_state = residency; assert!(liveness_details(&record).is_none()); } }
#[test] fn healthy_member_has_no_liveness_notification() { let mut record = record(); record.status = TaskStatus::Completed; assert!(liveness_details(&record).is_none()); }
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
    let calls=Arc::new(AtomicUsize::new(0)); let delivered=calls.clone(); let notifier=create_team_member_liveness_notifier(TeamMemberLivenessDeps { deliver:Arc::new(move |_,_| { delivered.fetch_add(1,Ordering::SeqCst); Ok(()) }),was_delivered:Arc::new(|_| false),mark_delivered:Arc::new(|_| panic!("not persisted")),on_error:Arc::new(|error| panic!("{error}")),timers:Arc::new(Timers::default()),max_delivery_retries:None,max_persistence_retries:None });
    let mut record=record(); notifier.notify_terminal(&record); notifier.notify_terminal(&record); assert_eq!(calls.load(Ordering::SeqCst),1); record.notification.run_epoch+=1; notifier.notify_terminal(&record); assert_eq!(calls.load(Ordering::SeqCst),2);
}
#[test] fn persisted_epoch_suppresses_redelivery() {
    let notifier=create_team_member_liveness_notifier(TeamMemberLivenessDeps { deliver:Arc::new(|_,_| panic!("already persisted")),was_delivered:Arc::new(|_| true),mark_delivered:Arc::new(|_| {}),on_error:Arc::new(|error| panic!("{error}")),timers:Arc::new(Timers::default()),max_delivery_retries:None,max_persistence_retries:None }); notifier.notify_terminal(&record());
}
#[test] fn failed_delivery_retries_exactly_to_configured_limit() {
    let calls=Arc::new(AtomicUsize::new(0)); let delivered=calls.clone(); let timers=Arc::new(Timers::default()); let notifier=create_team_member_liveness_notifier(TeamMemberLivenessDeps { deliver:Arc::new(move |_,_| { delivered.fetch_add(1,Ordering::SeqCst); Err("failure".into()) }),was_delivered:Arc::new(|_| false),mark_delivered:Arc::new(|_| {}),on_error:Arc::new(|_| {}),timers:timers.clone(),max_delivery_retries:Some(2),max_persistence_retries:None }); notifier.notify_terminal(&record()); timers.fire(); timers.fire(); assert_eq!(calls.load(Ordering::SeqCst),3); assert!(timers.0.lock().expect("callbacks").is_empty());
}
