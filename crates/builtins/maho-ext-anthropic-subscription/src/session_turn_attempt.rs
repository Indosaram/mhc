use crate::{
    session_continuity::Snapshot, session_reattach::BindingStore, session_registry::SessionEntry,
};
use serde_json::Value;

pub fn binding_from_entry(entry: &SessionEntry, hashes: &[String]) -> Snapshot {
    Snapshot {
        sdk_session_id: entry.pump.sdk_session_id.clone(),
        account_name: entry.account_name.clone(),
        model_id: entry.model_id.clone(),
        system_prompt_hash: entry.system_prompt_hash.clone(),
        toolset_hash: entry.toolset_hash.clone(),
        sent_count: hashes.len(),
        sent_hashes: hashes.to_vec(),
        last_assistant_uuid: entry.assistant_uuid_by_index.get(&hashes.len()).cloned(),
        assistant_uuid_by_index: entry.assistant_uuid_by_index.clone(),
        pending_fork_reason: entry.pending_fork_reason.clone(),
        tainted_reason: entry.tainted_reason.clone(),
        sdk_session_id_confirmed: Some(entry.pump.sdk_session_id_confirmed),
        credential_digest: entry.credential_digest.clone(),
        ..Default::default()
    }
}

/// Claude Code's wording for a fork point absent from its transcript; the captured id is the dead one.
static RESUME_MESSAGE_MISSING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)no message found with message\.uuid(?:\s+of)?:\s*([0-9a-fA-F-]+)")
        .expect("resume message pattern")
});

/// senpi `withoutMissingAssistantUuid`: drops only the boundary Claude Code rejected, so the next admission does not
/// ask for the same dead id again (senpi#1958); earlier boundaries stay mapped.
pub fn without_missing_assistant_uuid(mut binding: Snapshot, message: &str) -> Snapshot {
    let Some(missing) = RESUME_MESSAGE_MISSING
        .captures(message)
        .and_then(|captures| captures.get(1))
        .map(|uuid| uuid.as_str().to_owned())
    else {
        return binding;
    };
    if binding.last_assistant_uuid.as_deref() == Some(missing.as_str()) {
        binding.last_assistant_uuid = None;
    }
    binding
        .assistant_uuid_by_index
        .retain(|_, uuid| *uuid != missing);
    binding
}

/// senpi `rememberUnansweredAttempt`: an unanswered attempt on an account other than the one `served_lineage` names
/// served nothing, so the session's prompt cache is most likely still on that account. Once the attempt's process is
/// gone, `served_lineage` stays the binding, so the next request prefers its account, forks its own lineage there and
/// reads its cache, and is not reported as a seat change (senpi#4304). An attempt on that same account keeps its own
/// retry checkpoint.
pub fn unanswered_attempt_binding(
    checkpoint: Snapshot,
    served_lineage: Option<Snapshot>,
) -> Snapshot {
    match served_lineage {
        Some(served) if served.account_name != checkpoint.account_name => served,
        _ => checkpoint,
    }
}

/// senpi `successfulTurn` plus the abort check: the attempt was answered.
pub fn successful_turn(turn: &crate::session_registry_pump::TurnResult) -> bool {
    !turn.aborted
        && turn.messages.iter().any(|message| {
            message["type"] == "result"
                && message["subtype"] == "success"
                && crate::errors::sdk_result_failure(message).is_none()
        })
}

pub async fn run(
    entry: &mut SessionEntry,
    uuid: String,
    message: Value,
    hashes: &[String],
    timing: (u64, Option<&maho_ai::utils::abort::AbortSignal>),
    bindings: &mut BindingStore,
    mut deliver: impl FnMut(Value),
) -> anyhow::Result<crate::session_registry_pump::TurnResult> {
    let (now, signal) = timing;
    let count = hashes.len();
    let mut assistant_ids = Vec::new();
    let result = entry
        .turn(uuid, message, now, signal, |message| {
            if message["type"] == "assistant"
                && message
                    .get("parent_tool_use_id")
                    .is_some_and(Value::is_null)
                && let Some(uuid) = message["uuid"].as_str()
            {
                assistant_ids.push(uuid.to_owned());
            }
            deliver(message);
        })
        .await;
    for uuid in assistant_ids {
        entry.assistant_uuid_by_index.insert(count, uuid);
    }
    match &result {
        Ok(turn) if successful_turn(turn) => {
            entry.record_synced_stream(hashes);
            bindings.remember(&entry.session_id, &binding_from_entry(entry, hashes));
        }
        Err(error)
            if error
                .to_string()
                .to_lowercase()
                .contains("no conversation found with session id") =>
        {
            bindings.forget(&entry.session_id)
        }
        _ => {
            if entry.sent_hashes.len() <= hashes.len() {
                let mut binding = binding_from_entry(entry, &entry.sent_hashes);
                binding.unanswered_turn_digest = Some(
                    crate::session_sync::sent_hash_prefix_digest(hashes, hashes.len()),
                );
                bindings.remember(&entry.session_id, &binding);
            }
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(account: &str) -> Snapshot {
        Snapshot {
            sdk_session_id: format!("sdk-{account}"),
            account_name: account.into(),
            last_assistant_uuid: Some("a2".into()),
            assistant_uuid_by_index: [(1, "a1".to_owned()), (2, "a2".to_owned())].into(),
            ..Default::default()
        }
    }
    #[test]
    fn unanswered_attempt_on_another_account_keeps_the_served_lineage() {
        assert_eq!(
            unanswered_attempt_binding(snapshot("other"), Some(snapshot("holder"))).account_name,
            "holder"
        );
        assert_eq!(
            unanswered_attempt_binding(snapshot("holder"), Some(snapshot("holder"))).sdk_session_id,
            "sdk-holder"
        );
        assert_eq!(
            unanswered_attempt_binding(snapshot("other"), None).account_name,
            "other"
        );
    }
    #[test]
    fn missing_message_uuid_is_dropped_from_the_lineage() {
        let binding = without_missing_assistant_uuid(
            snapshot("holder"),
            "No message found with message.uuid of: a2",
        );
        assert_eq!(binding.last_assistant_uuid, None);
        assert_eq!(
            binding.assistant_uuid_by_index,
            [(1, "a1".to_owned())].into()
        );
        let unrelated = without_missing_assistant_uuid(snapshot("holder"), "some other failure");
        assert_eq!(unrelated.last_assistant_uuid.as_deref(), Some("a2"));
        assert_eq!(unrelated.assistant_uuid_by_index.len(), 2);
    }
}
