use super::*;

fn entry(kind: &str) -> Entry {
    Entry { id: 1, conversation_id: 2, kind: kind.into(), model: None, data: None,
        head: None, edits: None, by_task_id: None }
}

#[test]
fn witnesses_discriminate_when_entries_have_different_kinds() {
    let kinds = entries();
    let witnesses = [kinds.user.kind(), kinds.assistant.kind(), kinds.tool_result.kind(),
        kinds.system.kind(), kinds.notice.kind(), kinds.usage.kind(), kinds.summary.kind(),
        kinds.handoff.kind(), kinds.reset.kind()];
    let stored = witnesses.map(entry);
    let matches: Vec<_> = stored.iter().map(|value| [
        kinds.user.is(Some(value)), kinds.assistant.is(Some(value)), kinds.tool_result.is(Some(value)),
        kinds.system.is(Some(value)), kinds.notice.is(Some(value)), kinds.usage.is(Some(value)),
        kinds.summary.is(Some(value)), kinds.handoff.is(Some(value)), kinds.reset.is(Some(value)),
    ]).collect();
    for (index, row) in matches.iter().enumerate() {
        for (column, matched) in row.iter().enumerate() { assert_eq!(*matched, index == column); }
    }
    assert!(!kinds.user.is(None));
}

#[test]
fn typed_entry_rejects_missing_model_when_assistant() {
    let json = r#"{"id":1,"conversationId":2,"kind":"pi.assistant","data":{"attempt":1}}"#;
    let result = serde_json::from_str::<AssistantEntry>(json);
    assert!(result.is_err());
}

#[test]
fn typed_entry_preserves_required_head_when_summary() {
    let json = r#"{"id":1,"conversationId":2,"kind":"pi.summary","head":8,"model":[{"role":"user","content":"summary","timestamp":0}],"data":{"through":7}}"#;
    let typed: SummaryEntry = serde_json::from_str(json).expect("summary");
    let serialized: Entry = serde_json::from_str(&serde_json::to_string(&typed).expect("serialize")).expect("entry");
    assert_eq!(serialized.head, Some(8));
    assert_eq!(serialized.model.expect("model").len(), 1);
}
