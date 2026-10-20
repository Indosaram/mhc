use super::*;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};
use crate::harness::context::background_context;
use super::super::super::hooks::{HookRunner, HookRunners};

fn task(kind: &str) -> Task {
    Task { id: 4, conversation_id: 2, kind: kind.into(), input: JsonValue::Null,
        status: TaskStatus::Running, checkpoint: None, abort: None, outcome: None,
        after: Vec::new(), owns: Vec::new(), background: None }
}

#[test]
fn rejects_reference_when_stored_kind_differs() {
    let stored = task("worker.actual");
    let reference = define_task("worker.other").expect("kind");
    let result = check_kind(&stored, &reference);
    assert!(matches!(result, Err(Error::Forbidden(_))));
}

#[test]
fn accepts_reference_when_stored_kind_matches() {
    let stored = task("worker.actual");
    let reference = define_task("worker.actual").expect("kind");
    let result = check_kind(&stored, &reference);
    assert!(result.is_ok());
}

#[test]
fn decodes_missing_record_when_commit_returns_null() {
    let committed = JsonValue::Null;
    let result = decode::<Option<Task>>(committed).expect("optional task");
    assert!(result.is_none());
}

#[test]
fn preserves_slot_json_when_returned_from_transaction() {
    let slot: JsonObject = serde_json::from_str(r#"{"memos":{"winner":null},"progress":"ready"}"#).expect("slot");
    let committed = to_stored(&Some(slot.clone())).expect("stored slot").as_json().clone();
    let result = decode::<Option<JsonObject>>(committed).expect("slot snapshot");
    assert_eq!(result, Some(slot));
}

pub(crate) struct AdapterRuntime {
    pub tx: RefCell<AdapterTx>,
    pub log: RefCell<Vec<String>>,
    pub plugins: BTreeMap<String, Rc<dyn PluginHandler>>,
    kinds: BTreeMap<String, KindToken>,
    hooks: HookRunner<()>,
    tools: BTreeMap<String, Rc<dyn AnyToolDeclaration>>,
}
impl AdapterRuntime {
    pub fn new() -> Self {
        let kind = define_task("worker.actual").expect("kind");
        Self {
            tx: RefCell::new(AdapterTx { current: task("pi.plugin"), tasks: BTreeMap::from([(4, task("worker.actual"))]),
                inputs: BTreeMap::new(), slots: BTreeMap::new(), checkpoints: Vec::new(), created: Vec::new(), snapshots: RefCell::new(Vec::new()) }),
            log: RefCell::new(Vec::new()), plugins: BTreeMap::new(),
            hooks: HookRunners::new(Vec::new, |_| Vec::new(), |_| {}).runner(kind.clone(), 4, 2),
            kinds: BTreeMap::from([(kind.name().into(), kind)]), tools: BTreeMap::new(),
        }
    }
    fn reference(&self) -> TaskRef { TaskRef { id: 4, kind: self.kinds["worker.actual"].clone() } }
}
pub(crate) struct AdapterTx {
    current: Task,
    tasks: BTreeMap<Id, Task>,
    inputs: BTreeMap<Id, Input>,
    slots: BTreeMap<String, JsonObject>,
    pub checkpoints: Vec<Checkpoint>,
    created: Vec<TaskOptions>,
    snapshots: RefCell<Vec<Id>>,
}
impl TxReads for AdapterTx {
    fn conversation(&self, _: Id) -> LocalBoxFuture<'_, Result<Option<Conversation>>> { unimplemented!("not used by adapter") }
    fn entry(&self, _: Id) -> LocalBoxFuture<'_, Result<Option<Entry>>> { unimplemented!("not used by adapter") }
    fn entries<'a>(&'a self, _: &'a [Id]) -> LocalBoxFuture<'a, Result<BTreeMap<Id, Entry>>> { unimplemented!("not used by adapter") }
    fn newest_entry(&self, _: EntryScan) -> LocalBoxFuture<'_, Result<Option<Entry>>> { unimplemented!("not used by adapter") }
    fn scan_entries(&self, _: EntryScan) -> LocalBoxFuture<'_, Result<Vec<Entry>>> { unimplemented!("not used by adapter") }
    fn context(&self, _: Id, _: Option<Id>) -> LocalBoxFuture<'_, Result<ContextView>> { unimplemented!("not used by adapter") }
    fn task(&self, id: Id) -> LocalBoxFuture<'_, Result<Option<Task>>> { Box::pin(async move { Ok(self.tasks.get(&id).cloned()) }) }
    fn tasks(&self, _: TaskScan) -> LocalBoxFuture<'_, Result<Vec<Task>>> { unimplemented!("not used by adapter") }
    fn input(&self, id: Id) -> LocalBoxFuture<'_, Result<Option<Input>>> { Box::pin(async move { Ok(self.inputs.get(&id).cloned()) }) }
    fn rewindable_as_of(&self, _: Id, _: Id) -> LocalBoxFuture<'_, Result<Option<RewindableState>>> { unimplemented!("not used by adapter") }
    fn snapshot(&self, reference: DocRef) -> Result<DocumentSnapshot> {
        let DocRef::Sticky { conversation_id } = reference else { panic!("sticky only"); };
        self.snapshots.borrow_mut().push(conversation_id);
        let mut sticky: StickyState = serde_json::from_str(r#"{"retry":{"enabled":false,"maxRetries":0,"baseDelayMs":0},"steeringMode":"all","followUpMode":"all","inbox":[],"turn":{"tools":[]},"tasks":{},"plugins":{}}"#)?;
        sticky.tasks = self.slots.clone();
        Ok(DocumentSnapshot::Sticky(sticky))
    }
}
impl SharedTx for AdapterTx {
    fn plugins(&self, _: &Namespace) -> Result<super::super::super::membrane::Document> { unimplemented!("not used by adapter") }
    fn emit_plugin(&mut self, _: &Namespace, _: &str, _: JsonValue) -> Result<()> { unimplemented!("not used by adapter") }
    fn config_get(&self, _: Id, _: &str) -> Result<Option<JsonValue>> { unimplemented!("not used by adapter") }
    fn write(&mut self, _: Id, _: NewEntry) -> LocalBoxFuture<'_, Result<Id>> { unimplemented!("not used by adapter") }
    fn create_task(&mut self, kind: &KindToken, _: JsonValue, opts: TaskOptions) -> Result<TaskRef> {
        self.created.push(opts);
        Ok(TaskRef { id: 10, kind: kind.clone() })
    }
    fn create_conversation(&mut self, _: ConversationSpec) -> Result<Id> { unimplemented!("owned runtime path required") }
}
impl TaskTx for AdapterTx {
    fn checkpoint(&mut self, value: Checkpoint) -> Result<()> { self.checkpoints.push(value); Ok(()) }
    fn slot(&self, _: &TaskRef) -> Result<super::super::super::membrane::Document> { unimplemented!("snapshot path required") }
}
impl Runtime<(), dyn TaskTx> for AdapterRuntime {
    fn task_id(&self) -> Id { 4 }
    fn conversation_id(&self) -> Id { 2 }
    fn kind(&self) -> &KindToken { &self.kinds["worker.actual"] }
    fn commit<'a>(&'a self, callback: CommitCallback<'a, dyn TaskTx>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<JsonValue>> {
        Box::pin(async move {
            self.log.borrow_mut().push("commit".into());
            let mut tx = std::mem::replace(&mut *self.tx.borrow_mut(), AdapterRuntime::new().tx.into_inner());
            let current = tx.current.clone();
            let result = callback(&mut tx, &current, ctx).await;
            self.tx.replace(tx);
            result
        })
    }
    fn hooks(&self) -> &HookRunner<()> { &self.hooks }
    fn models(&self) -> &dyn Models { unimplemented!("not used by adapter") }
    fn tools(&self) -> &BTreeMap<String, Rc<dyn AnyToolDeclaration>> { &self.tools }
    fn kinds(&self) -> &BTreeMap<String, KindToken> { &self.kinds }
    fn registries(&self) -> RuntimeRegistries<'_> { unimplemented!("not used by adapter") }
    fn process_host(&self) -> Option<&dyn ProcessHost> { None }
    fn plugins(&self) -> &BTreeMap<String, Rc<dyn PluginHandler>> { &self.plugins }
    fn now(&self) -> u64 { 0 }
    fn sleep<'a>(&'a self, _: u64, _: &'a Context) -> LocalBoxFuture<'a, Result<()>> { unimplemented!("no timers") }
    fn wait_for_input<'a>(&'a self, id: Id, _: &'a Context) -> LocalBoxFuture<'a, Result<Input>> {
        Box::pin(async move { Ok(self.tx.borrow().inputs.get(&id).expect("input").clone()) })
    }
    fn wait_for_task<'a>(&'a self, id: Id, _: &'a Context) -> LocalBoxFuture<'a, Result<Task>> {
        Box::pin(async move { Ok(self.tx.borrow().tasks.get(&id).expect("task").clone()) })
    }
    fn abort_task<'a>(&'a self, _: Id, _: &'a Context) -> LocalBoxFuture<'a, Result<AbortTaskResult>> { unimplemented!("not used by adapter") }
    fn abort_conversation<'a>(&'a self, id: Id, _: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move { self.log.borrow_mut().push(format!("abort:{id}")); Ok(()) })
    }
    fn create_owned_conversation<'a>(&'a self, _: OwnedConversationSpec, _: &'a Context) -> LocalBoxFuture<'a, Result<Id>> {
        Box::pin(async move { self.log.borrow_mut().push("owned".into()); Ok(9) })
    }
    fn send_owned<'a>(&'a self, id: Id, _: SendInput, _: &'a Context) -> LocalBoxFuture<'a, Result<Id>> {
        Box::pin(async move {
            self.log.borrow_mut().push(format!("send:{id}"));
            self.tx.borrow_mut().inputs.insert(11, Input { id: 11, conversation_id: id, request_id: None, status: InputStatus::Queued,
                entry: None, answer: None, reason: None, detail: None });
            Ok(11)
        })
    }
    fn context<'a>(&'a self, _: Id, _: Option<Id>, _: &'a Context) -> LocalBoxFuture<'a, Result<ContextView>> { unimplemented!("not used by adapter") }
    fn newest_entry<'a>(&'a self, _: Id, _: NewestEntryOptions, _: &'a Context) -> LocalBoxFuture<'a, Result<Option<Entry>>> { unimplemented!("not used by adapter") }
    fn rewindable<'a>(&'a self, _: Id, _: &'a Context) -> LocalBoxFuture<'a, Result<RewindableState>> { unimplemented!("not used by adapter") }
    fn sticky<'a>(&'a self, _: Id, _: &'a Context) -> LocalBoxFuture<'a, Result<StickyState>> { unimplemented!("must snapshot on line") }
    fn rewindable_as_of<'a>(&'a self, _: Id, _: Id, _: &'a Context) -> LocalBoxFuture<'a, Result<Option<RewindableState>>> { unimplemented!("not used by adapter") }
}

#[tokio::test(flavor = "current_thread")]
async fn tool_only_operations_reject_when_used_by_task() {
    let runtime = AdapterRuntime::new();
    let ctx = background_context();
    let current = task("worker.actual");
    let api = task_api(&current, &runtime);
    let results = [api.stream(b"output"), api.progress(Box::new(|_| panic!("must not invoke update")), &ctx).await,
        api.memo("key", Some(JsonValue::Null), &ctx).await.map(|_| ())];
    assert!(results.iter().all(|result| matches!(result, Err(Error::Forbidden(_)))));
    assert!(runtime.log.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn slot_snapshots_stored_conversation_when_target_is_child() {
    let runtime = AdapterRuntime::new();
    runtime.tx.borrow_mut().tasks.get_mut(&4).expect("task").conversation_id = 9;
    let slot: JsonObject = serde_json::from_str(r#"{"progress":"child"}"#).expect("slot");
    runtime.tx.borrow_mut().slots.insert("4".into(), slot.clone());
    let current = task("worker.actual");
    let api = task_api(&current, &runtime);
    let result = api.slot(&runtime.reference(), &background_context()).await.expect("slot");
    assert_eq!(result, Some(slot));
    assert_eq!(*runtime.tx.borrow().snapshots.borrow(), vec![9]);
}

#[tokio::test(flavor = "current_thread")]
async fn task_rejects_forged_same_name_token_before_commit() {
    let runtime = AdapterRuntime::new();
    let current = task("worker.actual");
    let api = task_api(&current, &runtime);
    let forged = define_task("worker.actual").expect("token");
    let result = api.task(&forged, JsonValue::Null, TaskOptions::default(), &background_context()).await;
    assert!(matches!(result, Err(Error::Forbidden(_))));
    assert!(runtime.log.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn task_discards_conversation_override_when_creating_child() {
    let runtime = AdapterRuntime::new();
    let current = task("worker.actual");
    let api = task_api(&current, &runtime);
    let options = TaskOptions { conversation_id: Some(999), after: vec![7], background: Some(True) };
    let result = api.task(&runtime.reference().kind, JsonValue::Null, options, &background_context()).await.expect("child");
    assert_eq!(result.id, 10);
    let tx = runtime.tx.borrow();
    assert_eq!(tx.created[0].conversation_id, None);
    assert_eq!(tx.created[0].after, vec![7]);
    assert_eq!(tx.created[0].background, Some(True));
}

#[tokio::test(flavor = "current_thread")]
async fn owned_handle_routes_send_wait_result_and_abort_through_runtime() {
    let runtime = AdapterRuntime::new();
    let current = task("worker.actual");
    let api = task_api(&current, &runtime);
    let ctx = background_context();
    let conversation = api.conversation(OwnedConversationSpec::default(), &ctx).await.expect("conversation");
    let input = conversation.send(SendInput { content: serde_json::from_str(r#""hello""#).expect("content"), request_id: None, when_busy: None }, &ctx).await.expect("send");
    assert_eq!(input.wait(&ctx).await.expect("wait").id, 11);
    assert_eq!(input.result(&ctx).await.expect("result").expect("input").conversation_id, 9);
    conversation.abort(&ctx).await.expect("abort");
    assert_eq!(*runtime.log.borrow(), vec!["owned", "send:9", "commit", "abort:9"]);
}
