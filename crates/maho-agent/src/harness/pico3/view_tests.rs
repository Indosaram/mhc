use super::*;
fn initial() -> ConversationView {
    ConversationView { conversation: ConversationInfo { id: 1, parent: None, owner: None }, entries: Vec::new(), config: JsonObject::new(), inbox: Vec::new(), turn: None, compaction: None, tasks: BTreeMap::new(), plugins: BTreeMap::new() }
}
fn entry(id: Id, head: Option<Id>) -> Entry { Entry { id, conversation_id: 1, kind: "note".into(), model: None, data: None, head, edits: None, by_task_id: None } }
#[test]
fn watch_buffers_exact_capacity_and_overflow_closes_only_itself() {
    let reports = Rc::new(RefCell::new(Vec::new())); let sink = reports.clone();
    let stopped = Rc::new(Cell::new(0)); let count = stopped.clone();
    let watch = Watch::new(initial(), 7, Rc::new(move |error| sink.borrow_mut().push(error.to_string())), move || count.set(count.get() + 1));
    for revision in 1..=WATCH_CAPACITY { watch.accept(Envelope { revision: u64::try_from(revision).unwrap(), ops: Vec::new(), events: Vec::new() }); }
    assert!(!watch.closed()); assert_eq!(watch.revision(), 7);
    watch.accept(Envelope { revision: 257, ops: Vec::new(), events: Vec::new() });
    assert!(watch.closed()); assert_eq!(stopped.get(), 1); assert_eq!(reports.borrow().len(), 1);
    watch.stop(); assert_eq!(stopped.get(), 1);
}
#[test]
fn start_delivers_buffer_in_order_and_listener_failure_is_contained() {
    let watch = Watch::new(initial(), 0, Rc::new(|_| {}), || {});
    for revision in 1..=3 { watch.accept(Envelope { revision, ops: Vec::new(), events: Vec::new() }); }
    let seen = Rc::new(RefCell::new(Vec::new())); let output = seen.clone();
    watch.start(Rc::new(move |e| { output.borrow_mut().push(e.revision); Ok(()) }));
    watch.start(Rc::new(|_| panic!("must not replace listener")));
    assert_eq!(*seen.borrow(), vec![1, 2, 3]);
    watch.stop(); watch.accept(Envelope { revision: 4, ops: Vec::new(), events: Vec::new() });
    assert_eq!(seen.borrow().len(), 3);
    let bad = Watch::new(initial(), 0, Rc::new(|_| {}), || {});
    bad.start(Rc::new(|_| Err(Error::InvalidShape("listener error".into()))));
    bad.accept(Envelope { revision: 1, ops: Vec::new(), events: Vec::new() }); assert!(bad.closed());
}
#[test]
fn head_truncation_retains_every_entry_inside_display_range() {
    let mut entries = vec![entry(1, None), entry(2, Some(2)), entry(3, None)];
    apply_entries(&mut entries, &[entry(4, None), entry(5, Some(3))]);
    assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), vec![3, 4, 5]);
    apply_entries(&mut entries, &[entry(6, Some(4))]);
    assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), vec![4, 5, 6]);
}
#[test]
fn envelope_applies_public_splices_without_changing_snapshot() {
    let mut view = initial(); view.entries = vec![entry(1, None), entry(2, None)];
    let envelope = Envelope { revision: 8, ops: vec![maho_chord::delta::Op::Splice(vec!["entries".into()], 0, 1, Vec::new())], events: Vec::new() };
    assert_eq!(apply_envelope(&view, &envelope).unwrap().entries[0].id, 2);
    assert_eq!(view.entries.len(), 2);
}
#[test]
fn generation_projection_covers_wait_retry_deferred_and_streaming() {
    let mut task = Task { id: 2, conversation_id: 1, kind: "pi.generation".into(), input: JsonValue::Null, status: TaskStatus::Pending, checkpoint: None, abort: None, outcome: None, after: vec![3], owns: Vec::new(), background: None };
    assert!(matches!(generation_status(&task, false), GenerationStatus::Waiting { .. }));
    task.status = TaskStatus::Running; task.after.clear();
    let mut fields = JsonObject::new(); fields.insert("attempt".into(), encode(&2_u64).unwrap()); fields.insert("untilMs".into(), encode(&100_u64).unwrap());
    task.checkpoint = Some(Checkpoint { phase: "retrying".into(), fields });
    assert!(matches!(generation_status(&task, false), GenerationStatus::Retrying { attempt: 2, retry_at: 100, .. }));
    task.checkpoint.as_mut().unwrap().phase = "requesting".into();
    assert!(matches!(generation_status(&task, true), GenerationStatus::Streaming { attempt: 2 }));
    task.checkpoint.as_mut().unwrap().phase = "deferred".into();
    assert!(matches!(generation_status(&task, false), GenerationStatus::Deferred { attempt: 2, .. }));
}
