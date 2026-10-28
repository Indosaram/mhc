//! Stable-id admission of input removed during move release.
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
pub async fn requeue_input(file: &Path, move_id: &str, texts: &[String]) -> Result<(), String> {
    let key = format!("pending-input:{}", file.display());
    crate::session_sidecar_store::serialize_by_key(&key, async {
        let entries = super::entries(file);
        let records: Vec<_> = entries.iter().filter(|e| e["customType"] == "pending_input_transition").collect();
        let mut cursor = records.iter().filter_map(|e| e["data"]["cursor"].as_u64()).max().unwrap_or(0);
        let lifetime = uuid::Uuid::new_v4().to_string();
        for (index, text) in texts.iter().enumerate() {
            let id = format!("move:{move_id}:{index}");
            let fingerprint = hex::encode(Sha256::digest(json!(["followUp",text,[]]).to_string().as_bytes()));
            if let Some(record) = records.iter().find(|e| e["data"]["input"]["clientMessageId"] == id) {
                if record["data"]["input"]["fingerprint"] != fingerprint { return Err(format!("client_message_id_conflict: {id}")); }
                continue;
            }
            cursor += 1;
            super::append_custom(file, "pending_input_transition", json!({"type":"pending_input_transition", "cursor":cursor,
                "input":{"clientMessageId":id,"fingerprint":fingerprint,"handle":{"lifetimeId":lifetime,"queuedId":format!("move-{move_id}-{index}")},"revision":1,"hasEdits":false,"text":text,"attachments":[],"requestedMode":"followUp","phase":"queued","attemptCount":0,"superseded":[],"operations":{}}}))?;
        }
        Ok(())
    }).await
}
