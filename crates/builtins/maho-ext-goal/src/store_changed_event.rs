pub const GOAL_STORE_CHANGED_EVENT: &str = "goal_store_changed";
pub struct GoalStoreChangedEvent { pub thread_id: String, pub ctx: Option<maho_ext_api::ExtensionContext> }
pub fn is_goal_store_changed_event(data: &serde_json::Value) -> bool { data.get("threadId").and_then(serde_json::Value::as_str).is_some_and(|id| !id.is_empty()) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn nonempty_thread_id_is_accepted() { let data = serde_json::json!({"threadId":"thread"}); let result = is_goal_store_changed_event(&data); assert!(result); }
    #[test] fn invalid_thread_ids_are_rejected() { let data = [serde_json::Value::Null, serde_json::json!({}), serde_json::json!({"threadId":""}), serde_json::json!({"threadId":1})]; let result = data.map(|d| is_goal_store_changed_event(&d)); assert_eq!(result, [false; 4]); }
}
