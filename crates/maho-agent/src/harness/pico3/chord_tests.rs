use super::*;
use crate::harness::context::background_context;
use maho_chord::types::ReplicatedState;
fn initial() -> ConversationView {
    ConversationView { conversation: ConversationInfo { id: 1, parent: None, owner: None }, entries: Vec::new(), config: JsonObject::new(), inbox: Vec::new(), turn: None, compaction: None, tasks: std::collections::BTreeMap::new(), plugins: std::collections::BTreeMap::new() }
}
#[test]
fn descriptors_preserve_exact_local_and_remote_service_contracts() {
    let host = pico_harness_service().unwrap(); let conversation = pico_conversation_service().unwrap();
    assert_eq!(host.id, JsString::from("pi.harness")); assert!(host.local);
    assert_eq!(conversation.id, JsString::from("pi.conversation")); assert!(!conversation.local);
}
#[test]
fn tracked_bridge_applies_utf16_splice_permutation_and_rejects_root_replace() {
    let value = JsonValue::object(vec![("text".into(), JsonValue::String(JsString::from_utf16(vec![0xd800]))), ("array".into(), JsonValue::array(vec![JsonValue::Null, JsonValue::Bool(true)]))]);
    let tracker = maho_chord::delta::track(value).unwrap(); let mut change = tracker.begin_change();
    let draft = change.state().unwrap();
    apply_tracked(&draft, &[Op::Append(vec!["text".into()], JsString::from_utf16(vec![0xdc00])), Op::Permute(vec!["array".into()], vec![1, 0])]).unwrap();
    let JsonValue::String(text) = draft.at(&["text".into()]).unwrap().value().unwrap() else { panic!("not string") };
    assert_eq!(text.units(), vec![0xd800, 0xdc00]);
    assert_eq!(draft.at(&["array".into(), 0_usize.into()]).unwrap().value().unwrap(), JsonValue::Bool(true));
    assert!(apply_tracked(&draft, &[Op::Replace(JsonValue::Null)]).is_err());
    assert!(apply_tracked(&draft, &[Op::Permute(vec!["array".into()], vec![0, 0])]).is_err());
    change.abort();
}
#[tokio::test(flavor = "current_thread")]
async fn changed_envelopes_publish_and_deeply_equal_events_normalize() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context();
        let watch = Watch::new(initial(), 0, Rc::new(|_| {}), || {});
        let initial = view::encode(&PublishedConversationView { view: initial(), commit: PublishedCommit { events: Vec::new() } }).unwrap();
        let state = maho_chord::services::state::MutableReplicatedStateImpl::new(initial).unwrap();
        let bridge = bridge_watch(watch.clone(), state.clone(), ctx.clone(), ChordViewBridgeOptions::default());
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        let dispose = state.subscribe(Box::new(move |_, _, delivery| { let send = send.clone(); Box::pin(async move { send.send(delivery.sequence).map_err(|e| Box::new(e) as ServiceError)?; Ok(()) }) }));
        tokio::time::timeout(std::time::Duration::from_secs(5), receive.recv()).await.unwrap().unwrap();
        for revision in 1..=2 {
            watch.accept(Envelope { revision, ops: vec![Op::Set(vec!["config".into(), "profile".into()], JsonValue::String(format!("profile-{revision}").into()))], events: Vec::new() });
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async { receive.recv().await.unwrap(); receive.recv().await.unwrap(); }).await.unwrap();
        for revision in 3..=4 {
            watch.accept(Envelope { revision, ops: Vec::new(), events: vec![ViewEvent::Core(CoreViewEvent::Warning { source: "test".into(), message: "repeat".into() })] });
        }
        bridge.drain();
        tokio::time::timeout(std::time::Duration::from_secs(5), receive.recv()).await.unwrap().unwrap();
        assert!(matches!(receive.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        let published: PublishedConversationView = view::decode(&state.value().unwrap()).unwrap();
        assert_eq!(published.view.config.get("profile"), Some(&JsonValue::String("profile-2".into())));
        bridge.close(); assert!(watch.closed()); dispose().await.unwrap();
    }).await;
}
struct FailingState;
impl ReplicatedState<JsonValue> for FailingState {
    fn value(&self) -> Option<Arc<JsonValue>> { None }
    fn subscribe(&self, _: maho_chord::types::StateListener<JsonValue>) -> maho_chord::types::Disposal { Box::new(|| Box::pin(async { Ok(()) })) }
}
impl MutableReplicatedState for FailingState {
    fn change(&self, _: maho_chord::types::ContextRef, _: &mut dyn FnMut(&mut maho_chord::delta::Change) -> std::result::Result<(), ServiceError>) -> std::result::Result<(), ServiceError> { Err(Box::new(std::io::Error::other("publication failure"))) }
    fn replace(&self, _: maho_chord::types::ContextRef, _: JsonValue) -> std::result::Result<(), ServiceError> { Err(Box::new(std::io::Error::other("publication failure"))) }
}
#[tokio::test(flavor = "current_thread")]
async fn publication_failure_closes_only_bridge_and_preserves_typed_cause() {
    tokio::task::LocalSet::new().run_until(async {
        let watch = Watch::new(initial(), 0, Rc::new(|_| {}), || {});
        let (send, receive) = tokio::sync::oneshot::channel(); let send = RefCell::new(Some(send));
        let bridge = bridge_watch(watch.clone(), Arc::new(FailingState), background_context(), ChordViewBridgeOptions { capacity: 256, on_failure: Rc::new(move |error| {
            let Error::Chord(cause) = error else { panic!("lost typed Chord cause") };
            assert!(cause.downcast_ref::<std::io::Error>().is_some());
            if let Some(send) = send.borrow_mut().take() { let _ = send.send(()); }
        }) });
        watch.accept(Envelope { revision: 1, ops: Vec::new(), events: Vec::new() });
        tokio::time::timeout(std::time::Duration::from_secs(5), receive).await.unwrap().unwrap();
        assert!(bridge.closed()); assert!(watch.closed());
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn queue_overflow_closes_raw_watch_and_reports_once() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context(); let watch = Watch::new(initial(), 0, Rc::new(|_| {}), || {});
        let state = maho_chord::services::state::MutableReplicatedStateImpl::new(view::encode(&PublishedConversationView { view: initial(), commit: PublishedCommit { events: Vec::new() } }).unwrap()).unwrap();
        let failures = Rc::new(Cell::new(0)); let output = failures.clone();
        let bridge = bridge_watch(watch.clone(), state, ctx, ChordViewBridgeOptions { capacity: 1, on_failure: Rc::new(move |_| output.set(output.get() + 1)) });
        for revision in 1..=2 { watch.accept(Envelope { revision, ops: Vec::new(), events: Vec::new() }); }
        assert!(bridge.closed()); assert!(watch.closed()); assert_eq!(failures.get(), 1); bridge.close();
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn service_commands_cannot_abort_inputs_or_read_entries_from_other_conversation() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context(); let h = super::super::harness::tests::open().await;
        let root = h.root(&ctx).await.unwrap(); let other = h.create_conversation(ConversationSpec::default(), None, &ctx).await.unwrap();
        let input = other.send(SendInput { content: maho_ai::types::UserContent::Text("other".into()), request_id: None, when_busy: None }, &ctx).await.unwrap();
        let bridge = attach_chord_view(&root, &ctx, ChordViewBridgeOptions::default()).await.unwrap();
        let capture = root.watch(&ctx).await.unwrap();
        let state = maho_chord::services::state::MutableReplicatedStateImpl::new(view::encode(&PublishedConversationView { view: capture.view().clone(), commit: PublishedCommit { events: Vec::new() } }).unwrap()).unwrap(); capture.stop();
        let service = create_pico_conversation_service(h.clone(), root.clone(), state);
        assert!(service.input_abort(input.id, &ctx).await.is_err());
        let id = root.write(NewEntry { kind: "note".into(), model: None, data: None, head: None, edits: None }, &ctx).await.unwrap();
        let written = h.session.read(&ctx, |storage, ctx| Box::pin(async move { storage.input(id, &ctx).await })).await.unwrap().unwrap();
        let entry_id = written.entry.expect("completed write has an entry");
        let entries = service.entries(EntryScan { conversation_id: other.id, kind: None, with_head: None, before: None, limit: 10 }, &ctx).await.unwrap();
        assert!(entries.iter().any(|entry| entry.id == entry_id)); assert!(entries.iter().all(|entry| entry.conversation_id == root.id));
        bridge.close(); h.close(&ctx).await.unwrap();
    }).await;
}
