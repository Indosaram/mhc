use serde_json::Value;
use crate::{session_registry::SessionEntry, session_continuity::Snapshot, session_reattach::BindingStore};

pub fn binding_from_entry(entry: &SessionEntry, hashes: &[String]) -> Snapshot {
    Snapshot {
        sdk_session_id: entry.pump.sdk_session_id.clone(), account_name: entry.account_name.clone(), model_id: entry.model_id.clone(),
        system_prompt_hash: entry.system_prompt_hash.clone(), toolset_hash: entry.toolset_hash.clone(), sent_count: hashes.len(), sent_hashes: hashes.to_vec(),
        last_assistant_uuid: entry.assistant_uuid_by_index.get(&hashes.len()).cloned(), assistant_uuid_by_index: entry.assistant_uuid_by_index.clone(),
        pending_fork_reason: entry.pending_fork_reason.clone(), tainted_reason: entry.tainted_reason.clone(), sdk_session_id_confirmed: Some(entry.pump.sdk_session_id_confirmed),
        ..Default::default()
    }
}

pub async fn run(entry: &mut SessionEntry, uuid: String, message: Value, hashes: &[String], timing: (u64, Option<&maho_ai::utils::abort::AbortSignal>), bindings: &mut BindingStore, mut deliver: impl FnMut(Value)) -> anyhow::Result<crate::session_registry_pump::TurnResult> {
    let (now, signal) = timing;
    let count = hashes.len();
    let mut assistant_ids = Vec::new();
    let result = entry.turn(uuid, message, now, signal, |message| {
        if message["type"] == "assistant" && message.get("parent_tool_use_id").is_some_and(Value::is_null) && let Some(uuid) = message["uuid"].as_str() {
            assistant_ids.push(uuid.to_owned());
        }
        deliver(message);
    }).await;
    for uuid in assistant_ids { entry.assistant_uuid_by_index.insert(count, uuid); }
    match &result {
        Ok(turn) if !turn.aborted && turn.messages.iter().any(|message| message["type"] == "result" && message["subtype"] == "success" && crate::errors::sdk_result_failure(message).is_none()) => {
            entry.record_synced_stream(hashes);
            bindings.remember(&entry.session_id, &binding_from_entry(entry, hashes));
        },
        Err(error) if error.to_string().to_lowercase().contains("no conversation found with session id") => bindings.forget(&entry.session_id),
        _ => {
            if entry.sent_hashes.len() <= hashes.len() {
                let mut binding = binding_from_entry(entry, &entry.sent_hashes);
                binding.unanswered_turn_digest = Some(crate::session_sync::sent_hash_prefix_digest(hashes, hashes.len()));
                bindings.remember(&entry.session_id, &binding);
            }
        },
    }
    result
}
