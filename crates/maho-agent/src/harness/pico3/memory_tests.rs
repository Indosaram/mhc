use super::*;
use crate::harness::context::BACKGROUND_CONTEXT;

fn writes(source: &str) -> Vec<Write> { serde_json::from_str(source).expect("fixture writes") }

#[tokio::test]
async fn rejected_batch_changes_no_table_document_sequence_or_high_water() {
    let storage = MemoryStorage::new();
    let ctx = &BACKGROUND_CONTEXT;
    let batch = writes(r#"[
        {"type":"conversation","conversation":{"id":40}},
        {"type":"doc","ref":{"doc":"session"},"ops":[["r",{"plugins":{"leaked":true}}]]},
        {"type":"entry","entry":{"id":40,"conversationId":40,"kind":"duplicate"}}
    ]"#);
    assert!(storage.commit(&batch, ctx).await.is_err());
    assert!(storage.conversation(40, ctx).await.expect("read").is_none());
    assert!(storage.entries(&[40], ctx).await.expect("read").is_empty());
    assert_eq!(serde_json::to_string(&storage.doc(DocRef::Session, ctx).await.expect("read")).expect("JSON"), r#"{"plugins":{}}"#);
    assert_eq!(storage.commit(&writes(r#"[{"type":"conversation","conversation":{"id":1}}]"#), ctx).await.expect("commit"), 1);
    assert_eq!(storage.mint_id().expect("id"), 2);
}

#[tokio::test]
async fn invalid_delta_does_not_publish_earlier_staged_writes() {
    let storage = MemoryStorage::new();
    let ctx = &BACKGROUND_CONTEXT;
    let batch = writes(r#"[
        {"type":"conversation","conversation":{"id":20}},
        {"type":"doc","ref":{"doc":"session"},"ops":[["s",["missing","nested"],true]]}
    ]"#);
    assert!(storage.commit(&batch, ctx).await.is_err());
    assert_eq!(storage.mint_id().expect("id"), 1);
    assert!(storage.conversations(ctx).await.expect("read").is_empty());
}

#[tokio::test]
async fn six_write_families_preserve_snapshots_requests_and_checkpoint_null() {
    let storage = MemoryStorage::new();
    let ctx = &BACKGROUND_CONTEXT;
    let batch = writes(r#"[
        {"type":"conversation","conversation":{"id":1,"sections":[{"key":"x","value":{"nested":1}}]}},
        {"type":"entry","entry":{"id":2,"conversationId":1,"kind":"note","data":{"nested":{"value":1}}}},
        {"type":"task","task":{"id":3,"conversationId":1,"kind":"task","input":{"nested":1},"status":"pending","after":[],"owns":[]}},
        {"type":"task.patch","patch":{"id":3,"status":"running","checkpoint":{"phase":"first"}}},
        {"type":"input","input":{"id":4,"conversationId":1,"status":"queued","requestId":"r"}},
        {"type":"doc","ref":{"doc":"rewindable","conversationId":1},"ops":[["r",{"plugins":{"p":{"nested":{"value":1}}}}]]}
    ]"#);
    storage.commit(&batch, ctx).await.expect("commit");
    let mut entry = storage.entries(&[2], ctx).await.expect("read").remove(&2).expect("entry");
    entry.kind = "mutated".into();
    let mut input = storage.input_by_request(1, "r", ctx).await.expect("read").expect("input");
    input.status = InputStatus::Done;
    let mut task = storage.task(3, ctx).await.expect("read").expect("task");
    task.input = JsonValue::Null;
    let mut doc = storage.doc(DocRef::Rewindable { conversation_id: 1 }, ctx).await.expect("read").expect("doc");
    doc.insert("plugins".into(), JsonValue::Null);
    assert_eq!(storage.entries(&[2], ctx).await.expect("read")[&2].kind, "note");
    assert_eq!(storage.input_by_request(1, "r", ctx).await.expect("read").expect("input").status, InputStatus::Queued);
    assert_ne!(storage.task(3, ctx).await.expect("read").expect("task").input, JsonValue::Null);
    assert_ne!(storage.doc(DocRef::Rewindable { conversation_id: 1 }, ctx).await.expect("read").expect("doc").get("plugins"), Some(&JsonValue::Null));
    storage.commit(&writes(r#"[{"type":"task.patch","patch":{"id":3,"owns":[1]}}]"#), ctx).await.expect("patch");
    assert!(storage.task(3, ctx).await.expect("read").expect("task").checkpoint.is_some());
    storage.commit(&writes(r#"[{"type":"task.patch","patch":{"id":3,"checkpoint":null}},{"type":"input","input":{"id":4,"conversationId":1,"requestId":"r","status":"done"}}]"#), ctx).await.expect("patch");
    assert!(storage.task(3, ctx).await.expect("read").expect("task").checkpoint.is_none());
    assert_eq!(storage.input_by_request(1, "r", ctx).await.expect("read").expect("input").status, InputStatus::Done);
    assert!(storage.input_by_request(9, "r", ctx).await.expect("read").is_none());
}

#[tokio::test]
async fn fork_scans_and_commit_granular_history_follow_ancestor_caps() {
    let storage = MemoryStorage::new();
    let ctx = &BACKGROUND_CONTEXT;
    storage.commit(&writes(r#"[
        {"type":"conversation","conversation":{"id":1}},
        {"type":"entry","entry":{"id":2,"conversationId":1,"kind":"note"}},
        {"type":"doc","ref":{"doc":"rewindable","conversationId":1},"ops":[["r",{"version":"same-commit-final"}]]}
    ]"#), ctx).await.expect("commit");
    storage.commit(&writes(r#"[
        {"type":"entry","entry":{"id":3,"conversationId":1,"kind":"later"}},
        {"type":"doc","ref":{"doc":"rewindable","conversationId":1},"ops":[["s",["version"],"later"]]},
        {"type":"conversation","conversation":{"id":10,"parent":{"conversationId":1,"at":2}}},
        {"type":"entry","entry":{"id":11,"conversationId":10,"kind":"summary","head":2}}
    ]"#), ctx).await.expect("commit");
    let scan = EntryScan { conversation_id: 10, kind: None, with_head: None, before: None, limit: 10 };
    assert_eq!(storage.scan_entries(scan, ctx).await.expect("scan").iter().map(|entry| entry.id).collect::<Vec<_>>(), vec![11, 2]);
    let historical = storage.doc_as_of(10, 2, ctx).await.expect("history").expect("doc");
    assert_eq!(historical.get("version").and_then(JsonValue::as_str), Some("same-commit-final"));
    assert!(storage.doc_as_of(99, 2, ctx).await.expect("history").is_none());
}

#[tokio::test]
async fn sticky_truncation_keeps_last_base_and_rewindable_keeps_history() {
    let storage = MemoryStorage::new();
    let ctx = &BACKGROUND_CONTEXT;
    for value in ["old", "new"] {
        let mut object = JsonObject::new();
        object.insert("value".into(), JsonValue::String(value.into()));
        let ops = vec![Op::Replace(JsonValue::object(object.into_inner()))];
        storage.commit(&[Write::Doc { r#ref: DocRef::Sticky { conversation_id: 1 }, ops: ops.clone() }, Write::Doc { r#ref: DocRef::Rewindable { conversation_id: 1 }, ops }], ctx).await.expect("commit");
    }
    storage.truncate(DocRef::Sticky { conversation_id: 1 }, ctx).await.expect("truncate");
    assert_eq!(storage.state.borrow().sticky[&1].len(), 1);
    assert_eq!(storage.state.borrow().rewindable[&1].len(), 2);
    assert_eq!(storage.doc(DocRef::Sticky { conversation_id: 1 }, ctx).await.expect("read").expect("doc").get("value").and_then(JsonValue::as_str), Some("new"));
}

#[tokio::test]
async fn safe_integer_boundary_and_closed_commit_are_explicit() {
    let storage = MemoryStorage::new();
    let ctx = &BACKGROUND_CONTEXT;
    assert!(storage.commit(&writes(r#"[{"type":"conversation","conversation":{"id":9007199254740992}}]"#), ctx).await.is_err());
    storage.commit(&writes(r#"[{"type":"conversation","conversation":{"id":9007199254740991}}]"#), ctx).await.expect("safe boundary");
    assert!(storage.mint_id().is_err());
    storage.close(ctx).await.expect("close");
    assert!(storage.commit(&[], ctx).await.is_err());
}
