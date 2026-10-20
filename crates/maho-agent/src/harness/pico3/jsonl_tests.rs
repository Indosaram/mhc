use super::*;
use crate::harness::context::BACKGROUND_CONTEXT;

fn writes(source: &str) -> Vec<Write> { serde_json::from_str(source).expect("fixture") }
fn record(seq: Seq, max_id: Id, writes: Vec<Write>, refs: Vec<String>) -> Record { Record { seq, max_id, writes, refs } }
fn write_records(path: &Path, records: &[Record]) {
    let mut bytes = Vec::new();
    for record in records { bytes.extend(serde_json::to_vec(record).expect("JSON")); bytes.push(b'\n'); }
    fs::write(path, bytes).expect("write fixture");
}
fn initial() -> Vec<Write> { writes(r#"[
    {"type":"conversation","conversation":{"id":1}},
    {"type":"task","task":{"id":2,"conversationId":1,"kind":"task","input":null,"status":"pending","after":[],"owns":[]}},
    {"type":"doc","ref":{"doc":"sticky","conversationId":1},"ops":[["r",{"value":"initial"}]]}
]"#) }

#[tokio::test]
async fn durable_batches_reopen_and_retire_live_task_sidecars() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = &BACKGROUND_CONTEXT;
    let storage = JsonlStorage::open(dir.path(), true).expect("open");
    storage.commit(&initial(), ctx).await.expect("create");
    storage.commit(&writes(r#"[{"type":"task.patch","patch":{"id":2,"status":"running","checkpoint":{"phase":"started"}}}]"#), ctx).await.expect("checkpoint");
    assert!(dir.path().join("task-2.jsonl").exists());
    storage.close(ctx).await.expect("close");
    let storage = JsonlStorage::open(dir.path(), false).expect("reopen");
    assert_eq!(storage.task(2, ctx).await.expect("read").expect("task").checkpoint.expect("checkpoint").phase, "started");
    storage.commit(&writes(r#"[{"type":"task.patch","patch":{"id":2,"status":"terminal","checkpoint":null,"outcome":{"status":"completed","result":{"retained":true}}}}]"#), ctx).await.expect("terminal");
    assert!(!dir.path().join("task-2.jsonl").exists());
    storage.close(ctx).await.expect("close");
    let storage = JsonlStorage::open(dir.path(), false).expect("reopen terminal");
    assert_eq!(storage.task(2, ctx).await.expect("read").expect("task").status, TaskStatus::Terminal);
    assert_eq!(storage.mint_id().expect("id"), 3);
}

#[tokio::test]
async fn unconfirmed_sidecars_and_torn_main_tail_are_removed_before_sequence_reuse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = &BACKGROUND_CONTEXT;
    let storage = JsonlStorage::open(dir.path(), false).expect("open");
    storage.commit(&initial(), ctx).await.expect("create");
    storage.close(ctx).await.expect("close");
    let checkpoint = writes(r#"[{"type":"task.patch","patch":{"id":2,"status":"running","checkpoint":{"phase":"unpublished"}}}]"#);
    write_records(&dir.path().join("task-2.jsonl"), &[record(2, 999_999, checkpoint, Vec::new())]);
    let mut file = append_file(&dir.path().join("main.jsonl")).expect("append");
    file.write_all(b"{\"seq\":2,\"maxId\":999999").expect("torn tail");
    drop(file);
    let storage = JsonlStorage::open(dir.path(), false).expect("recover");
    assert_eq!(storage.task(2, ctx).await.expect("read").expect("task").status, TaskStatus::Pending);
    assert_eq!(storage.mint_id().expect("id"), 3);
    assert_eq!(fs::metadata(dir.path().join("task-2.jsonl")).expect("metadata").len(), 0);
    assert_eq!(storage.commit(&writes(r#"[{"type":"task.patch","patch":{"id":2,"status":"running","checkpoint":{"phase":"confirmed"}}}]"#), ctx).await.expect("reuse seq"), 2);
    storage.close(ctx).await.expect("close");
    let storage = JsonlStorage::open(dir.path(), false).expect("recover again");
    assert_eq!(storage.task(2, ctx).await.expect("read").expect("task").checkpoint.expect("checkpoint").phase, "confirmed");
}

#[test]
fn missing_published_sidecar_and_malformed_complete_records_fail_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_records(&dir.path().join("main.jsonl"), &[record(1, 1, Vec::new(), vec!["sticky-1.jsonl".into()])]);
    assert!(JsonlStorage::open(dir.path(), false).is_err());
    write_records(&dir.path().join("main.jsonl"), &[record(1, 0, Vec::new(), Vec::new()), record(1, 0, Vec::new(), Vec::new())]);
    assert!(JsonlStorage::open(dir.path(), false).is_err());
    fs::write(dir.path().join("main.jsonl"), b"{\"seq\":\"x\"}\n").expect("malformed");
    assert!(JsonlStorage::open(dir.path(), false).is_err());
}

#[tokio::test]
async fn failed_second_sidecar_does_not_publish_first_sidecar_or_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = &BACKGROUND_CONTEXT;
    let storage = JsonlStorage::open(dir.path(), false).expect("open");
    storage.commit(&initial(), ctx).await.expect("create");
    fs::create_dir(dir.path().join("sticky-9.jsonl")).expect("block second sidecar");
    let batch = writes(r#"[
        {"type":"task.patch","patch":{"id":2,"status":"running","checkpoint":{"phase":"unpublished"}}},
        {"type":"doc","ref":{"doc":"sticky","conversationId":9},"ops":[["r",{"value":"unpublished"}]]}
    ]"#);
    assert!(storage.commit(&batch, ctx).await.is_err());
    assert_eq!(storage.task(2, ctx).await.expect("read").expect("task").status, TaskStatus::Pending);
    storage.close(ctx).await.expect("close");
    fs::remove_dir(dir.path().join("sticky-9.jsonl")).expect("remove injected blocker");
    let storage = JsonlStorage::open(dir.path(), false).expect("recover");
    assert_eq!(storage.task(2, ctx).await.expect("read").expect("task").status, TaskStatus::Pending);
    assert!(storage.doc(DocRef::Sticky { conversation_id: 9 }, ctx).await.expect("read").is_none());
}

#[tokio::test]
async fn sticky_rewrite_preserves_publication_history_and_lossless_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ctx = &BACKGROUND_CONTEXT;
    let storage = JsonlStorage::open(dir.path(), true).expect("open");
    storage.commit(&initial(), ctx).await.expect("create");
    let batch = writes(r#"[{"type":"doc","ref":{"doc":"sticky","conversationId":1},"ops":[["r",{"\ud800":"\udfff","version":"new"}]]}]"#);
    storage.commit(&batch, ctx).await.expect("new base");
    let before = fs::read(dir.path().join("main.jsonl")).expect("main");
    storage.truncate(DocRef::Sticky { conversation_id: 1 }, ctx).await.expect("truncate");
    assert_eq!(fs::read(dir.path().join("main.jsonl")).expect("main"), before);
    assert_eq!(read_records(&dir.path().join("sticky-1.jsonl")).expect("records").len(), 1);
    let expected = storage.doc(DocRef::Sticky { conversation_id: 1 }, ctx).await.expect("read");
    storage.close(ctx).await.expect("close");
    let storage = JsonlStorage::open(dir.path(), false).expect("reopen");
    assert_eq!(storage.doc(DocRef::Sticky { conversation_id: 1 }, ctx).await.expect("read"), expected);
}

#[test]
fn confirmed_record_after_unconfirmed_tail_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_records(&dir.path().join("main.jsonl"), &[record(2, 0, Vec::new(), vec!["sticky-1.jsonl".into()])]);
    write_records(&dir.path().join("sticky-1.jsonl"), &[record(1, 0, Vec::new(), Vec::new()), record(2, 0, Vec::new(), Vec::new())]);
    assert!(JsonlStorage::open(dir.path(), false).is_err());
}
