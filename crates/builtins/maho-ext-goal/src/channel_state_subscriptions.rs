use std::sync::Arc;
use maho_ext_api::{EventBus,BusSubscription};
use serde_json::Value;
pub type WakeSourceCallback=Arc<dyn Fn(&str,f64,Option<&Value>)+Send+Sync>;
pub type ContinuationHoldCallback=Arc<dyn Fn(&str,bool)+Send+Sync>;
pub fn subscribe_goal_channel_state(events:&EventBus,on_wake_source:WakeSourceCallback,on_continuation_hold:ContinuationHoldCallback)->Vec<BusSubscription> {
    let monitor=Arc::clone(&on_wake_source);
    vec![
        events.on("terminal_monitor_state",Arc::new(move |data| { if let Some(count)=data.get("activeCount").and_then(Value::as_f64).filter(|count|count.is_finite() && count.fract()==0.0 && *count>=0.0) { monitor("terminal-monitors",count,None); } })),
        events.on("wake_source_state",Arc::new(move |data| { if let Some(source)=data.get("source").and_then(Value::as_str).filter(|source|!source.is_empty()) && let Some(count)=data.get("activeCount").and_then(Value::as_f64).filter(|count|count.is_finite()) { on_wake_source(source,count,Some(data)); } })),
        events.on("continuation_hold_state",Arc::new(move |data| { if let Some(source)=data.get("source").and_then(Value::as_str).filter(|source|!source.is_empty()) && let Some(active)=data.get("active").and_then(Value::as_bool) { on_continuation_hold(source,active); } })),
    ]
}
#[cfg(test)] mod tests {
    use super::*;
    use std::sync::Mutex;
    #[test] fn monitor_validates_integer_count_and_unsubscribes_on_drop() {
        let events=EventBus::default(); let seen=Arc::new(Mutex::new(Vec::new())); let captured=Arc::clone(&seen);
        let subscriptions=subscribe_goal_channel_state(&events,Arc::new(move |source,count,event|captured.lock().unwrap().push((source.to_owned(),count,event.is_some()))),Arc::new(|_,_|{}));
        events.emit("terminal_monitor_state",&serde_json::json!({"activeCount":1.5})); events.emit("terminal_monitor_state",&serde_json::json!({"activeCount":-1})); events.emit("terminal_monitor_state",&serde_json::json!({"activeCount":2}));
        assert_eq!(*seen.lock().unwrap(),[("terminal-monitors".into(),2.0,false)]); drop(subscriptions); events.emit("terminal_monitor_state",&serde_json::json!({"activeCount":3})); assert_eq!(seen.lock().unwrap().len(),1);
    }
    #[test] fn wake_preserves_fractional_counts_and_full_payload() {
        let events=EventBus::default(); let seen=Arc::new(Mutex::new(Vec::new())); let captured=Arc::clone(&seen);
        let _subscriptions=subscribe_goal_channel_state(&events,Arc::new(move |source,count,event|captured.lock().unwrap().push((source.to_owned(),count,event.cloned()))),Arc::new(|_,_|{}));
        let payload=serde_json::json!({"source":"task","activeCount":1.5,"items":[]}); events.emit("wake_source_state",&payload); events.emit("wake_source_state",&serde_json::json!({"source":"","activeCount":1})); assert_eq!(*seen.lock().unwrap(),[("task".into(),1.5,Some(payload))]);
    }
    #[test] fn hold_requires_boolean_not_truthy_string() {
        let events=EventBus::default(); let seen=Arc::new(Mutex::new(Vec::new())); let captured=Arc::clone(&seen);
        let _subscriptions=subscribe_goal_channel_state(&events,Arc::new(|_,_,_|{}),Arc::new(move |source,active|captured.lock().unwrap().push((source.to_owned(),active))));
        events.emit("continuation_hold_state",&serde_json::json!({"source":"guard","active":"true"})); events.emit("continuation_hold_state",&serde_json::json!({"source":"guard","active":true})); assert_eq!(*seen.lock().unwrap(),[("guard".into(),true)]);
    }
}
