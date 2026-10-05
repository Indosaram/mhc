use serde_json::{Value, json};
use crate::{session_binding_store::StoredBinding, session_continuity::Snapshot, session_commit_boundary::assistant_content_hash, session_sync::sent_hash_prefix_digest};

pub const BINDING_ENTRY_TYPE: &str = "claude-sdk-oauth-binding";
pub fn binding_marker() -> Value { json!({"schemaVersion":2,"marker":true}) }
pub struct StoredBindingAnchor<'a> {
    pub session_path: &'a str, pub session_id: &'a str,
    pub marker_entry_id: &'a str, pub assistant_content_hash: &'a str,
}
pub fn stored_binding_from_entry(entry: &Snapshot, hashes: &[String], anchor: StoredBindingAnchor<'_>) -> StoredBinding {
    StoredBinding {
        schema_version: 1, session_path: anchor.session_path.into(), session_id: anchor.session_id.into(),
        marker_entry_id: anchor.marker_entry_id.into(), sdk_session_id: entry.sdk_session_id.clone(),
        sent_count: hashes.len() as u64, sent_prefix_hash: sent_hash_prefix_digest(hashes, hashes.len()),
        assistant_content_hash: anchor.assistant_content_hash.into(),
        last_assistant_uuid: entry.assistant_uuid_by_index.get(&hashes.len()).cloned(),
        account_name: entry.account_name.clone(), model_id: entry.model_id.clone(),
        system_prompt_hash: entry.system_prompt_hash.clone(), toolset_hash: entry.toolset_hash.clone(),
    }
}
pub fn stored_binding_from_binding(binding: &Snapshot, hashes: &[String], anchor: StoredBindingAnchor<'_>) -> Option<StoredBinding> {
    if binding.sdk_session_id_confirmed == Some(false) || binding.sent_count != hashes.len() { return None; }
    let expected = sent_hash_prefix_digest(hashes, hashes.len());
    let digest = binding.sent_prefix_hash.clone().or_else(|| (!binding.sent_hashes.is_empty()).then(|| sent_hash_prefix_digest(&binding.sent_hashes, binding.sent_hashes.len())))?;
    if digest != expected { return None; }
    let mut stored = stored_binding_from_entry(binding, hashes, anchor);
    stored.last_assistant_uuid = binding.last_assistant_uuid.clone(); Some(stored)
}
fn newest_binding_entry(branch: &[Value]) -> Option<usize> {
    branch.iter().rposition(|entry| entry["type"] == "custom" && entry["customType"] == BINDING_ENTRY_TYPE)
}
pub fn invalidation_reason_from_branch(branch: &[Value]) -> Option<&str> {
    let data = &branch[newest_binding_entry(branch)?]["data"];
    if data["invalidated"] == true { data["reason"].as_str() } else { None }
}
fn ledger_only(entry: &Value) -> bool {
    match entry["type"].as_str() {
        Some("custom_message") => entry["customType"] == "goal-continuation",
        Some("custom" | "label" | "session_info" | "thinking_level_change" | "model_change" | "model_change_rejected" | "configuration_update") => true,
        _ => false,
    }
}
fn append_only(entry: &Value) -> bool {
    ledger_only(entry) || entry["type"] == "custom_message" || (entry["type"] == "message" && matches!(entry["message"]["role"].as_str(), Some("user" | "toolResult")))
}
pub fn binding_from_stored_branch(branch: &[Value], stored: &StoredBinding) -> Option<Snapshot> {
    let marker_index = newest_binding_entry(branch)?; let marker = &branch[marker_index];
    if marker["id"] != stored.marker_entry_id || marker["data"]["schemaVersion"] != 2 || marker["data"]["marker"] != true { return None; }
    let mut assistant_index = None;
    for (index, entry) in branch.iter().enumerate().skip(marker_index + 1) {
        if entry["type"] == "message" { assistant_index = Some(index); break; }
        if !ledger_only(entry) { return None; }
    }
    let assistant_index = assistant_index?;
    if !branch[assistant_index + 1..].iter().all(append_only) { return None; }
    let assistant = &branch[assistant_index]["message"];
    if assistant["role"] != "assistant" || !["api", "provider", "model"].iter().all(|key| assistant[*key].is_string()) || !assistant["content"].is_array() { return None; }
    if assistant_content_hash(assistant) != stored.assistant_content_hash { return None; }
    let count = usize::try_from(stored.sent_count).ok()?;
    Some(Snapshot {
        sdk_session_id: stored.sdk_session_id.clone(), sent_count: count,
        sent_prefix_hash: Some(stored.sent_prefix_hash.clone()), last_assistant_uuid: stored.last_assistant_uuid.clone(),
        assistant_uuid_by_index: stored.last_assistant_uuid.clone().map(|uuid| [(count, uuid)].into()).unwrap_or_default(),
        account_name: stored.account_name.clone(), model_id: stored.model_id.clone(),
        system_prompt_hash: stored.system_prompt_hash.clone(), toolset_hash: stored.toolset_hash.clone(), ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn assistant(text: &str) -> Value { json!({"role":"assistant","api":"claude-sdk-oauth","provider":"anthropic-subscription","model":"test","content":[{"type":"text","text":text}]}) }
    fn fixture() -> (Vec<Value>, StoredBinding) {
        let message = assistant("answer");
        let entry = Snapshot { sdk_session_id:"sdk".into(), account_name:"primary".into(), model_id:"test".into(), system_prompt_hash:"1".repeat(64), toolset_hash:"2".repeat(64), assistant_uuid_by_index:[(2,"a2".into())].into(), ..Default::default() };
        let stored = stored_binding_from_entry(&entry, &["h1".into(),"h2".into()], StoredBindingAnchor {session_path:"/tmp/session",session_id:"session",marker_entry_id:"marker",assistant_content_hash:&assistant_content_hash(&message)});
        (vec![json!({"type":"custom","id":"marker","customType":BINDING_ENTRY_TYPE,"data":binding_marker()}),json!({"type":"message","message":message})],stored)
    }
    #[test]
    fn restores_only_matching_marker_and_committed_assistant() {
        let (mut branch, stored) = fixture(); let restored = binding_from_stored_branch(&branch,&stored).expect("binding");
        assert_eq!(restored.sdk_session_id,"sdk"); assert_eq!(restored.sent_count,2); assert_eq!(restored.assistant_uuid_by_index[&2],"a2");
        branch[0]["id"] = json!("other"); assert!(binding_from_stored_branch(&branch,&stored).is_none());
        branch[0]["id"] = json!("marker"); branch[0]["data"] = json!({"schemaVersion":1,"sdkSessionId":"untrusted"}); assert!(binding_from_stored_branch(&branch,&stored).is_none());
    }
    #[test]
    fn rejects_rewritten_assistant_and_nonassistant_anchor() {
        let (mut branch, stored) = fixture(); branch[1]["message"] = assistant("rewritten"); assert!(binding_from_stored_branch(&branch,&stored).is_none());
        branch[1]["message"] = json!({"role":"user","content":"user"}); assert!(binding_from_stored_branch(&branch,&stored).is_none());
    }
    #[test]
    fn admits_unsent_messages_and_all_ledger_metadata_after_anchor() {
        for tail in [json!({"type":"message","message":{"role":"user"}}),json!({"type":"message","message":{"role":"toolResult"}}),json!({"type":"custom_message","customType":"notice"}),json!({"type":"custom_message","customType":"goal-continuation"}),json!({"type":"custom","customType":"unknown-extension"}),json!({"type":"model_change_rejected"})] {
            let (mut branch, stored) = fixture(); branch.push(tail); assert!(binding_from_stored_branch(&branch,&stored).is_some());
        }
    }
    #[test]
    fn rejects_assistant_compaction_and_branch_summary_tails() {
        for tail in [json!({"type":"message","message":assistant("later")}),json!({"type":"compaction"}),json!({"type":"branch_summary"})] {
            let (mut branch, stored) = fixture(); branch.push(tail); assert!(binding_from_stored_branch(&branch,&stored).is_none());
        }
    }
    #[test]
    fn permits_only_ledger_entries_between_marker_and_assistant() {
        for kind in ["custom","label","session_info","thinking_level_change","model_change","configuration_update"] {
            let (mut branch, stored) = fixture(); branch.insert(1,json!({"type":kind})); assert!(binding_from_stored_branch(&branch,&stored).is_some());
        }
        let (mut branch, stored) = fixture(); branch.insert(1,json!({"type":"custom_message","customType":"model-visible"})); assert!(binding_from_stored_branch(&branch,&stored).is_none());
    }
    #[test]
    fn newest_marker_retires_invalidation_and_newest_invalidation_refuses_binding() {
        let (mut branch, stored) = fixture(); branch.push(json!({"type":"custom","customType":BINDING_ENTRY_TYPE,"data":{"invalidated":true,"reason":"changed"}}));
        assert_eq!(invalidation_reason_from_branch(&branch),Some("changed")); assert!(binding_from_stored_branch(&branch,&stored).is_none());
        branch.push(branch[0].clone()); assert_eq!(invalidation_reason_from_branch(&branch),None);
    }
    #[test]
    fn fallback_binding_requires_confirmed_matching_prefix_not_just_count() {
        let hashes = vec!["h".into()]; let mut binding = Snapshot { sdk_session_id:"sdk".into(),sent_count:1,last_assistant_uuid:Some("a".into()),..Default::default() };
        let anchor = || StoredBindingAnchor {session_path:"/tmp/session",session_id:"session",marker_entry_id:"marker",assistant_content_hash:"hash"};
        assert!(stored_binding_from_binding(&binding,&hashes,anchor()).is_none()); binding.sent_hashes=hashes.clone();
        assert_eq!(stored_binding_from_binding(&binding,&hashes,anchor()).expect("stored").last_assistant_uuid,Some("a".into()));
        binding.sdk_session_id_confirmed=Some(false); assert!(stored_binding_from_binding(&binding,&hashes,anchor()).is_none());
        binding.sdk_session_id_confirmed=Some(true); binding.sent_prefix_hash=Some("different".into()); assert!(stored_binding_from_binding(&binding,&hashes,anchor()).is_none());
    }
}
