use std::future::Future;
pub const POST_COMPACTION_MONITOR_COUNT:u32=5;
pub const POST_COMPACTION_NO_TEXT_THRESHOLD:u64=3;
pub const MAX_RECOVERY_ATTEMPTS:u32=3;
pub const RECOVERY_INSTRUCTIONS:&str="RECOVERY: prior compaction caused degraded responses; rebuild context";
pub const RECOVERY_NOTIFICATION:&str="Detected repeated no-text assistant responses; retried compaction recovery.";
#[derive(Debug,Default,PartialEq,Eq)]
pub struct DegradationMonitorState {
    pub post_compaction_turns_remaining:u32,
    pub no_text_counter:u64,
    pub recovery_triggered_this_cycle:bool,
    pub recovery_attempts:u32,
}
pub struct MonitoredMessageContentPart<'a> {pub kind:&'a str,pub text:Option<&'a str>}
pub struct MonitoredMessageEvent<'a> {pub role:&'a str,pub content:&'a [MonitoredMessageContentPart<'a>]}
pub struct RecoveryResult {pub applied:bool,pub reason:String}
pub fn create_degradation_monitor_state()->DegradationMonitorState {DegradationMonitorState::default()}
pub fn reset_on_session_compact(state:&mut DegradationMonitorState) {
    state.post_compaction_turns_remaining=POST_COMPACTION_MONITOR_COUNT;
    state.no_text_counter=0;
    state.recovery_triggered_this_cycle=false;
}
pub async fn handle_message_end<F,Fut,N>(state:&mut DegradationMonitorState,event:MonitoredMessageEvent<'_>,apply_compaction:F,notify:N)
where F:FnOnce(&'static str)->Fut,Fut:Future<Output=RecoveryResult>,N:FnOnce(&'static str) {
    if state.post_compaction_turns_remaining==0 {return;}
    let has_text=event.role!="assistant" || event.content.iter().any(|part|part.kind=="text" && part.text.is_some_and(|text|!text.is_empty()));
    if has_text {state.no_text_counter=0;return;}
    state.no_text_counter=state.no_text_counter.saturating_add(1);
    if state.no_text_counter<POST_COMPACTION_NO_TEXT_THRESHOLD || state.recovery_triggered_this_cycle || state.recovery_attempts>=MAX_RECOVERY_ATTEMPTS {return;}
    state.recovery_triggered_this_cycle=true;
    state.recovery_attempts=state.recovery_attempts.saturating_add(1);
    state.no_text_counter=0;
    let result=apply_compaction(RECOVERY_INSTRUCTIONS).await;
    if result.applied || result.reason!="failed" {notify(RECOVERY_NOTIFICATION);}
}
pub fn handle_turn_end(state:&mut DegradationMonitorState) {
    state.post_compaction_turns_remaining=state.post_compaction_turns_remaining.saturating_sub(1);
}
