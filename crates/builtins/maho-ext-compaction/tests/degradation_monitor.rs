use maho_ext_compaction::degradation_monitor::*;
use std::cell::Cell;
fn state()->DegradationMonitorState {let mut state=create_degradation_monitor_state();reset_on_session_compact(&mut state);state}
async fn deliver(state:&mut DegradationMonitorState,text:Option<&str>,applied:bool,reason:&str)->(usize,usize) {
    let calls=Cell::new(0);let notices=Cell::new(0);
    let content=[MonitoredMessageContentPart {kind:if text.is_some(){"text"}else{"step-start"},text}];
    handle_message_end(state,MonitoredMessageEvent {role:"assistant",content:&content},|_|{calls.set(calls.get()+1);async {RecoveryResult {applied,reason:reason.to_owned()}}},|_|notices.set(notices.get()+1)).await;
    (calls.get(),notices.get())
}
#[tokio::test] async fn text_does_not_increment_counter() {let mut s=state();assert_eq!(deliver(&mut s,Some("answer"),true,"ok").await,(0,0));assert_eq!(s.no_text_counter,0);}
#[tokio::test] async fn first_no_text_message_counts_once() {let mut s=state();assert_eq!(deliver(&mut s,None,true,"ok").await,(0,0));assert_eq!(s.no_text_counter,1);}
#[tokio::test] async fn third_no_text_message_triggers_recovery() {let mut s=state();s.no_text_counter=2;assert_eq!(deliver(&mut s,None,true,"ok").await.0,1);assert!(s.recovery_triggered_this_cycle);}
#[tokio::test] async fn text_resets_consecutive_counter() {let mut s=state();s.no_text_counter=2;deliver(&mut s,Some("answer"),true,"ok").await;assert_eq!(s.no_text_counter,0);}
#[tokio::test] async fn recovery_clears_counter_and_notifies() {let mut s=state();s.no_text_counter=2;assert_eq!(deliver(&mut s,None,true,"ok").await,(1,1));assert_eq!(s.no_text_counter,0);}
#[tokio::test] async fn recovery_never_recurses_within_cycle() {let mut s=state();s.no_text_counter=2;s.recovery_attempts=1;s.recovery_triggered_this_cycle=true;assert_eq!(deliver(&mut s,None,true,"ok").await,(0,0));assert_eq!(s.recovery_attempts,1);}
#[tokio::test] async fn outside_monitor_window_does_not_track() {let mut s=state();s.post_compaction_turns_remaining=0;assert_eq!(deliver(&mut s,None,true,"ok").await,(0,0));assert_eq!(s.no_text_counter,0);}
#[tokio::test] async fn failed_recovery_does_not_duplicate_error_notice() {let mut s=state();s.no_text_counter=2;assert_eq!(deliver(&mut s,None,false,"failed").await,(1,0));}
#[tokio::test] async fn rejected_recovery_still_notifies() {let mut s=state();s.no_text_counter=2;assert_eq!(deliver(&mut s,None,false,"rejected").await,(1,1));}
#[test] fn new_cycle_preserves_session_recovery_ceiling() {let mut s=state();s.recovery_attempts=3;s.no_text_counter=2;s.recovery_triggered_this_cycle=true;reset_on_session_compact(&mut s);assert_eq!(s.recovery_attempts,3);assert_eq!(s.no_text_counter,0);assert!(!s.recovery_triggered_this_cycle);}
#[test] fn turn_end_expires_window_without_underflow() {let mut s=state();for _ in 0..6 {handle_turn_end(&mut s);}assert_eq!(s.post_compaction_turns_remaining,0);}
