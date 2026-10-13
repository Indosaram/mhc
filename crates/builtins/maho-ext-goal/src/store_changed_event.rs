pub const GOAL_STORE_CHANGED_EVENT: &str = "goal_store_changed";
pub struct GoalStoreChangedEvent { pub thread_id: String, pub ctx: Option<maho_ext_api::ExtensionContext> }
impl GoalStoreChangedEvent {
    pub fn from_json(data:&serde_json::Value)->Option<Self> {
        let thread_id=data.get("threadId").and_then(serde_json::Value::as_str).filter(|id|!id.is_empty())?;
        Some(Self { thread_id:thread_id.into(),ctx:None })
    }
    pub fn to_json(&self)->serde_json::Value { serde_json::json!({"threadId":self.thread_id}) }
}
pub fn is_goal_store_changed_event(data: &serde_json::Value) -> bool { data.get("threadId").and_then(serde_json::Value::as_str).is_some_and(|id| !id.is_empty()) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn nonempty_thread_id_is_accepted() { let data = serde_json::json!({"threadId":"thread"}); let result = is_goal_store_changed_event(&data); assert!(result); }
    #[test] fn invalid_thread_ids_are_rejected() { let data = [serde_json::Value::Null, serde_json::json!({}), serde_json::json!({"threadId":""}), serde_json::json!({"threadId":1})]; let result = data.map(|d| is_goal_store_changed_event(&d)); assert_eq!(result, [false; 4]); }
    #[test] fn json_round_trip_carries_thread_id_and_leaves_context_native() {
        let event=GoalStoreChangedEvent::from_json(&serde_json::json!({"threadId":"thread"})).unwrap();
        assert_eq!(event.thread_id,"thread"); assert!(event.ctx.is_none());
        assert_eq!(event.to_json(),serde_json::json!({"threadId":"thread"}));
        assert!(GoalStoreChangedEvent::from_json(&serde_json::json!({"threadId":""})).is_none());
        assert!(GoalStoreChangedEvent::from_json(&serde_json::Value::Null).is_none());
    }
}
