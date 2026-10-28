//! Per-item custody is never replaced with whole-batch payloads.
use crate::extensions::types::{SendMessageItem, SendMessageReceipt};
use serde_json::{Value, json};
pub fn item_notice_message(message: &Value, item: &SendMessageItem) -> Value {
    let mut r = json!({"role":"custom","customType":message["customType"],"content":item.content.as_ref().or_else(||message.get("content")).cloned().unwrap_or(json!([])),"display":message["display"],"timestamp":chrono::Utc::now().timestamp_millis()});
    if let Some(details) = &item.details {
        r["details"] = details.clone();
    }
    r
}
pub fn delivered_receipts(items: &[SendMessageItem], entry_id: &str) -> Vec<SendMessageReceipt> {
    items
        .iter()
        .map(|i| SendMessageReceipt {
            item_id: i.item_id.clone(),
            entry_id: entry_id.into(),
            disposition: "message_persisted".into(),
            notice_id: None,
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn item_notices_do_not_copy_batch_details() {
        let item = SendMessageItem {
            item_id: "a".into(),
            producer: "p".into(),
            run_ref: None,
            content: Some(json!("own")),
            details: None,
        };
        let notice = item_notice_message(
            &json!({"customType":"batch","content":"all","display":true,"details":{"private":"batch"}}),
            &item,
        );
        assert_eq!(notice["content"], "own");
        assert!(notice.get("details").is_none());
        let receipts = delivered_receipts(&[item], "e");
        assert_eq!(receipts[0].entry_id, "e");
    }
}
