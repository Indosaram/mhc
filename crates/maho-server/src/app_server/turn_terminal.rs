use serde_json::{Value,json};

pub fn turn_terminal_notifications(thread_id: &str,turn: Value) -> Vec<Value> {
    let mut notifications = Vec::new();
    if turn["status"] == "failed" && !turn["error"].is_null() {
        notifications.push(json!({"method":"error","params":{"threadId":thread_id,"turnId":turn["id"],"error":turn["error"],"willRetry":false}}));
    }
    notifications.push(json!({"method":"turn/completed","params":{"threadId":thread_id,"turn":turn}}));
    notifications
}
