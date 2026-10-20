use super::*;
use crate::harness::context::background_context;
use super::super::memory::MemoryStorage;
struct NoModels;
impl Models for NoModels {
    fn resolve(&self, _: &ModelRef) -> Option<Model> { None }
    fn stream(&self, _: &Model, _: RequestOptions, _: &Context) -> AssistantStream { panic!("no-model host must not stream") }
    fn fetch_deferred<'a>(&'a self, _: &'a Model, _: &'a DeferredHandle, _: &'a Context) -> LocalBoxFuture<'a, Result<DeferredResult>> { Box::pin(async { panic!("unexpected deferred fetch") }) }
    fn cancel_deferred<'a>(&'a self, _: &'a Model, _: &'a DeferredHandle, _: &'a Context) -> LocalBoxFuture<'a, Result<()>> { Box::pin(async { panic!("unexpected deferred cancel") }) }
}
pub(crate) async fn open() -> Rc<Harness> {
    Harness::open(Rc::new(MemoryStorage::new()), HarnessOptions::new(Rc::new(NoModels)), &background_context()).await.unwrap()
}
fn note() -> NewEntry { NewEntry { kind: "note".into(), model: None, data: None, head: None, edits: None } }
struct EchoPlugin;
impl PluginHandler for EchoPlugin {
    fn call<'a>(&'a self, input: JsonValue, _: &'a dyn ToolApi, _: &'a Context) -> LocalBoxFuture<'a, Result<JsonValue>> { Box::pin(async move { Ok(input) }) }
}
struct OrdinaryKind { token: KindToken, config: KindConfig }
impl Kind for OrdinaryKind {
    type Input = JsonValue; type Checkpoint = Checkpoint; type Result = JsonValue; type Failure = JsonValue;
    type Aborted = JsonValue; type Hooks = (); type Slot = JsonObject; type Tx = dyn TaskTx;
    fn token(&self) -> &KindToken { &self.token }
    fn turn(&self) -> bool { false }
    fn config(&self) -> &KindConfig { &self.config }
    fn inflight(&self) -> &[String] { &[] }
    fn slot(&self, _: &JsonValue) -> Result<JsonObject> { Ok(JsonObject::new()) }
    fn describe(&self, _: &Task, _: Option<&JsonObject>) -> Result<JsonValue> { Ok(JsonValue::Bool(true)) }
    fn initial<'a>(&'a self, _: &'a Task, rt: &'a dyn Runtime<(), dyn TaskTx>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Step>> {
        Box::pin(async move {
            rt.commit(Box::new(|tx, _, _| Box::pin(async move { tx.checkpoint(Checkpoint { phase: "finish".into(), fields: JsonObject::new() })?; Ok(JsonValue::Null) })), ctx).await?;
            Ok(Step::Next(Checkpoint { phase: "finish".into(), fields: JsonObject::new() }))
        })
    }
    fn phase<'a>(&'a self, phase: &'a str, _: &'a Task, _: &'a dyn Runtime<(), dyn TaskTx>, _: &'a Context) -> LocalBoxFuture<'a, Result<Step>> {
        Box::pin(async move {
            assert_eq!(phase, "finish");
            Ok(Step::Build(Box::new(|tx, _, _| Box::pin(async move { tx.write(1, note()).await?; Ok(Transition::Done(Completion::Completed { result: JsonValue::Bool(true) })) }))))
        })
    }
    fn abort<'a>(&'a self, _: &'a Task, _: &'a dyn Runtime<(), dyn TaskTx>, _: &'a Context) -> LocalBoxFuture<'a, Result<AbortClosure>> {
        Box::pin(async { Ok(Box::new(|_, _, _| Box::pin(async { Ok(JsonValue::Null) })) as AbortClosure) })
    }
}
#[tokio::test(flavor = "current_thread")]
async fn typed_adapter_executes_ordinary_runtime_builder_and_plugin_done() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context(); let mut options = HarnessOptions::new(Rc::new(NoModels)); options.plugins.insert("echo".into(), Rc::new(EchoPlugin));
        let h = Harness::open(Rc::new(MemoryStorage::new()), options, &ctx).await.unwrap();
        let kind = Rc::new(OrdinaryKind { token: define_task("ordinary").unwrap(), config: KindConfig::default() });
        let unregister = h.register_task_kind(kind, &["finish"], task_access).unwrap();
        let (ordinary, plugin) = h.session.commit(Invoker::Kernel { conversation_id: Some(1) }, |tx, _| Box::pin(async move {
            let ordinary = tx.create_task_spec(TaskSpec { kind: "ordinary".into(), input: JsonValue::Null, conversation_id: Some(1), after: None, background: None })?;
            let input = JsonValue::object(vec![("handler".into(), JsonValue::String("echo".into())), ("input".into(), JsonValue::Bool(true))]);
            let plugin = tx.create_task_spec(TaskSpec { kind: "pi.plugin".into(), input, conversation_id: Some(1), after: None, background: None })?; Ok((ordinary, plugin))
        }), &ctx, CommitOptions::default()).await.unwrap().value;
        h.resume(&ctx).await.unwrap();
        for id in [ordinary, plugin] {
            let task = tokio::time::timeout(Duration::from_secs(5), h.wait_for_task(id, &ctx)).await.unwrap().unwrap();
            assert!(matches!(task.outcome, Some(Outcome::Completed { result: JsonValue::Bool(true) })));
        }
        assert!(h.root(&ctx).await.unwrap().watch(&ctx).await.unwrap().view().entries.iter().any(|entry| entry.kind == "note"));
        unregister(); h.close(&ctx).await.unwrap();
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn real_host_installs_six_executable_kinds_and_atomically_folds_watches() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context(); let h = open().await; let root = h.root(&ctx).await.unwrap();
        assert_eq!(h.session.kinds.borrow().len(), 6); assert_eq!(h.kinds().unwrap().job.name(), "pi.job");
        let watch = root.watch(&ctx).await.unwrap(); let folded = Rc::new(RefCell::new(watch.view().clone())); let target = folded.clone();
        watch.start(Rc::new(move |envelope| { let next = apply_envelope(&target.borrow(), envelope)?; *target.borrow_mut() = next; Ok(()) }));
        for _ in 0..3 { root.write(note(), &ctx).await.unwrap(); }
        root.reset(Some("carry".into()), &ctx).await.unwrap(); root.write(note(), &ctx).await.unwrap();
        let fresh = root.watch(&ctx).await.unwrap();
        assert_eq!(view::encode(&*folded.borrow()).unwrap(), view::encode(fresh.view()).unwrap());
        assert_eq!(fresh.view().entries.len(), 2); fresh.stop(); watch.stop(); h.close(&ctx).await.unwrap();
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn namespace_identity_routes_and_unregister_are_authoritative() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context(); let h = open().await;
        let mut defaults = NamespaceDefaults::default(); defaults.session.insert("counter".into(), JsonValue::Null);
        let old = h.namespace("test.scope", defaults.clone(), None).unwrap();
        assert!(h.namespace("test.scope", defaults.clone(), None).is_err());
        old.unregister(); let current = h.namespace("test.scope", defaults, None).unwrap();
        assert!(!old.same_identity(&current));
        let kind = h.kinds().unwrap().plugin;
        assert!(h.hooks(&old, &kind, Rc::new(()), None, false).is_err());
        let off = h.hooks(&current, &kind, Rc::new(()), None, false).unwrap(); off(); off();
        let mut collision = NamespaceDefaults::default(); collision.session.insert("x".into(), JsonValue::Null); collision.sticky.insert("x".into(), JsonValue::Null);
        assert!(h.namespace("collision", collision, None).is_err());
        assert!(h.namespace("pi.invalid", NamespaceDefaults::default(), None).is_err());
        h.close(&ctx).await.unwrap();
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn no_model_send_runs_real_generation_and_settles_input() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context(); let h = open().await; h.resume(&ctx).await.unwrap();
        let root = h.root(&ctx).await.unwrap();
        let input = root.send(SendInput { content: maho_ai::types::UserContent::Text("hello".into()), request_id: Some("once".into()), when_busy: None }, &ctx).await.unwrap();
        let done = tokio::time::timeout(Duration::from_secs(5), input.wait(&ctx)).await.unwrap().unwrap();
        assert_eq!(done.status, InputStatus::Unanswered); assert_eq!(done.reason, Some(InputReason::Failed));
        h.close(&ctx).await.unwrap();
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn conversation_observers_are_off_line_and_unregister_is_idempotent() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context(); let h = open().await;
        let seen = Rc::new(RefCell::new(Vec::new())); let sink = seen.clone();
        let off = h.on_conversation(Rc::new(move |c| { sink.borrow_mut().push(c.id); Ok(()) }));
        let child = h.create_conversation(ConversationSpec::default(), None, &ctx).await.unwrap();
        assert_eq!(*seen.borrow(), vec![1, child.id]); off(); off();
        h.create_conversation(ConversationSpec::default(), None, &ctx).await.unwrap(); assert_eq!(seen.borrow().len(), 2);
        h.close(&ctx).await.unwrap();
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn held_send_is_deduplicated_and_fork_reset_preserve_parent_history() {
    tokio::task::LocalSet::new().run_until(async {
        let ctx = background_context(); let h = open().await; let hold = h.hold().unwrap(); h.resume(&ctx).await.unwrap();
        let root = h.root(&ctx).await.unwrap();
        let input = SendInput { content: maho_ai::types::UserContent::Text("hello".into()), request_id: Some("same".into()), when_busy: None };
        let first = root.send(input.clone(), &ctx).await.unwrap(); let second = root.send(input, &ctx).await.unwrap(); assert_eq!(first.id, second.id);
        let child = root.fork(ForkAt::Start(Start::Start), ConversationSpec::default(), &ctx).await.unwrap();
        child.reset(None, &ctx).await.unwrap();
        let original = root.watch(&ctx).await.unwrap(); let forked = child.watch(&ctx).await.unwrap();
        assert!(original.view().entries.iter().all(|entry| entry.kind != "pi.reset"));
        assert_eq!(forked.view().entries.last().unwrap().kind, "pi.reset");
        original.stop(); forked.stop(); drop(hold); h.close(&ctx).await.unwrap();
        assert!(root.write(note(), &ctx).await.is_err());
    }).await;
}
