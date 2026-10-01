use std::sync::{Arc,Mutex};
use maho_ext_api::{EventBus,BusSubscription};
use crate::monitor_continuation::MonitorAwareGoalContinuation;
pub fn subscribe_monitor_channels(events:&EventBus,monitor:Arc<Mutex<MonitorAwareGoalContinuation>>,now:Arc<dyn Fn()->f64+Send+Sync>,question_idle_ms:f64,changed:Arc<dyn Fn()+Send+Sync>)->Vec<BusSubscription> {
    let wake_monitor=monitor.clone(); let wake_now=now.clone(); let wake_changed=changed.clone();
    crate::channel_state_subscriptions::subscribe_goal_channel_state(events,Arc::new(move |source,count,event| {
        let deadlines=event.and_then(|event|event.get("items")).and_then(serde_json::Value::as_array).map_or_else(Vec::new,|items|items.iter().filter_map(|item|item.get("deadlineAtMs").and_then(serde_json::Value::as_f64)).collect::<Vec<_>>());
        wake_monitor.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set_wake_source_count(source,count,&deadlines,wake_now(),question_idle_ms); wake_changed();
    }),Arc::new(move |source,active| {
        let id=format!("external:{source}"); let mut monitor=monitor.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if active { monitor.hold_direct_input(&id,now()); } else { monitor.resolve_direct_input(&id,false,now()); }
        drop(monitor); changed();
    }))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn channels_drive_deadline_drain_and_external_hold_and_drop_unsubscribes() {
        let events=EventBus::default(); let monitor=Arc::new(Mutex::new(MonitorAwareGoalContinuation::default())); let changed=Arc::new(std::sync::atomic::AtomicUsize::new(0)); let captured=changed.clone();
        let subscriptions=subscribe_monitor_channels(&events,monitor.clone(),Arc::new(||0.0),30_000.0,Arc::new(move || { captured.fetch_add(1,std::sync::atomic::Ordering::SeqCst); }));
        events.emit("wake_source_state",&serde_json::json!({"source":"ask-user","activeCount":1,"items":[{"deadlineAtMs":5000}]})); assert_eq!(monitor.lock().unwrap().ask_user_deadline_at_ms,Some(5000.0));
        monitor.lock().unwrap().arm_timer(crate::wait_progress::GoalWaitKind::Monitor,10_000.0,10_000.0,false,0.0);
        events.emit("continuation_hold_state",&serde_json::json!({"source":"guard","active":true})); assert!(monitor.lock().unwrap().held_timer.is_some());
        events.emit("continuation_hold_state",&serde_json::json!({"source":"guard","active":false})); assert!(monitor.lock().unwrap().armed_timer.is_some());
        events.emit("wake_source_state",&serde_json::json!({"source":"ask-user","activeCount":0})); assert!(monitor.lock().unwrap().armed_timer.unwrap().drain_fire);
        assert_eq!(changed.load(std::sync::atomic::Ordering::SeqCst),4); drop(subscriptions);
        events.emit("wake_source_state",&serde_json::json!({"source":"task","activeCount":1})); assert_eq!(changed.load(std::sync::atomic::Ordering::SeqCst),4);
    }
}
