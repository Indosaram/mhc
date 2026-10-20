use serde_json::{Value, json};

pub fn unstarted_result(cell_id: &str, message: &str) -> Value {
    json!({"type":"result","cellId":cell_id,"ok":false,"error":{"message":message},"durationMs":0,"kernelState":"not-run"})
}
