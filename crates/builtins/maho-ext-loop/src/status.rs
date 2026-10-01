use crate::types::{CronEntry, LoopPhase, LoopState};
pub const LOOP_STATUS_KEY: &str = "loop";
pub const LOOP_STATUS_TICK_INTERVAL_MS: u64 = 1000;
fn format_duration(ms: f64) -> String {
    let seconds = (ms / 1000.0).floor().max(0.0);
    if seconds < 60.0 { return format!("{seconds}s"); }
    let minutes = (seconds / 60.0).floor();
    if minutes < 60.0 { return if seconds % 60.0 > 0.0 { format!("{minutes}m{}s",seconds % 60.0) } else { format!("{minutes}m") }; }
    let hours = (minutes / 60.0).floor();
    if hours < 24.0 { return if minutes % 60.0 > 0.0 { format!("{hours}h{}m",minutes % 60.0) } else { format!("{hours}h") }; }
    let days = (hours / 24.0).floor();
    if hours % 24.0 > 0.0 { format!("{days}d{}h",hours % 24.0) } else { format!("{days}d") }
}
pub fn format_loop_status(state: &LoopState, now_ms: f64) -> Option<String> {
    let armed: Vec<_> = state.entries.values().filter(|entry| match entry { CronEntry::Fixed { lifecycle, .. } | CronEntry::Dynamic { lifecycle, .. } => lifecycle.phase != LoopPhase::Ended }).collect();
    if armed.is_empty() { return None; }
    if armed.iter().any(|entry| match entry { CronEntry::Fixed { lifecycle, .. } | CronEntry::Dynamic { lifecycle, .. } => lifecycle.phase == LoopPhase::Suspended }) { return Some("Loop paused - /loop resume or /loop stop".into()); }
    let mut nearest = None;
    for entry in armed {
        let (mode, due) = match entry { CronEntry::Fixed { next_fire_at, .. } => ("fixed",Some(*next_fire_at)), CronEntry::Dynamic { pending_wakeup, .. } => ("dynamic",pending_wakeup.as_ref().map(|w| w.due_at)) };
        if let Some(due) = due && nearest.is_none_or(|(_, previous)| due < previous) { nearest = Some((mode,due)); }
    }
    Some(nearest.map_or_else(|| "Loop active - /loop stop".into(), |(mode,due)| format!("Loop ({mode}): next in {} - /loop stop",format_duration((due-now_ms).max(0.0)))))
}
pub fn format_noop_fold(noop_streak: f64) -> String { if noop_streak < 2.0 { String::new() } else { format!("\u{21bb} {noop_streak} loop ticks with no actionable change") } }
#[cfg(test)] mod tests {
    use super::*;
    fn state(phase: &str, due: f64) -> LoopState { serde_json::from_value(serde_json::json!({"version":1,"sessionId":"s","updatedAt":0,"activeDynamicId":null,"entries":{"a":{"id":"a","kind":"fixed","phase":phase,"originalArgs":"5m check","reentryPrompt":"/loop 5m check","payload":{"type":"prompt","prompt":"check"},"createdAt":0,"lastFiredAt":null,"expiresAt":1000000000000.0,"lastScheduledForAt":null,"coalescedFirePending":false,"queuedForAt":null,"noopStreak":0,"tickCount":0,"sentinelDelivery":{"autonomousPreambleDelivered":false,"lastLoopFileDelivered":null,"forceFullDelivery":false},"wakeSources":[],"requestedInterval":{"value":5,"unit":"m","raw":"5m"},"effectiveInterval":{"value":5,"unit":"m","human":"5 minutes","rounded":false},"cronExpression":"*/5 * * * *","nextFireAt":due,"intervalMs":300000}}})).unwrap() }
    #[test] fn nothing_armed_has_no_status() { let state=state("ended",60000.0); let result=format_loop_status(&state,0.0); assert!(result.is_none()); }
    #[test] fn fixed_countdown_is_selected() { let state=state("waiting",60000.0); let result=format_loop_status(&state,0.0).unwrap(); assert!(result.contains("fixed")); assert!(result.contains("1m")); }
    #[test] fn countdown_uses_supplied_clock() { let state=state("waiting",300000.0); let result=format_loop_status(&state,150000.0).unwrap(); assert!(result.contains("2m30s")); }
    #[test] fn dynamic_wakeup_countdown_is_selected() { let mut value=serde_json::to_value(state("waiting",0.0)).unwrap(); value["entries"]["a"]["kind"]=serde_json::json!("dynamic"); value["entries"]["a"]["keepaliveCredit"]=serde_json::json!(1); value["entries"]["a"]["pendingWakeup"]=serde_json::json!({"id":"w","loopId":"a","kind":"dynamic","source":"model","requestedDelaySeconds":120,"delaySeconds":120,"dueAt":120000,"reason":"continue","prompt":"/loop check","noop":false,"createdAt":0}); let state=serde_json::from_value(value).unwrap(); let result=format_loop_status(&state,0.0).unwrap(); assert!(result.contains("dynamic")); assert!(result.contains("2m")); }
    #[test] fn suspended_loop_has_pause_status() { let state=state("suspended",60000.0); let result=format_loop_status(&state,0.0).unwrap(); assert!(result.contains("paused")); }
    #[test] fn noops_below_two_are_not_folded() { let result=format_noop_fold(0.0); assert!(result.is_empty()); }
    #[test] fn noop_streak_is_exposed() { let result=format_noop_fold(3.0); assert!(result.contains('3')); }
    #[test] fn duration_retains_day_and_hour_parts() { let result=format_duration(90000000.0); assert_eq!(result,"1d1h"); }
}
