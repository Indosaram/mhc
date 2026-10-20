//! Serialized Pico3 transaction line and capability checked record overlays.
use std::{cell::{Cell, RefCell}, collections::{BTreeMap, BTreeSet}, rc::{Rc, Weak}, sync::LazyLock};
use serde::{Serialize, de::DeserializeOwned};
use crate::harness::context::{create_context_key, with_context_value, without_abort_signal};
use super::{types::*, legacy_tracker::{LegacyTracker, track}, membrane::{Document, Membrane}, context::derive_context};

pub type SlotFactory = Rc<dyn Fn(&JsonValue) -> Result<JsonObject>>;
pub type TaskDescriber = Rc<dyn Fn(&Task, Option<&JsonObject>) -> Result<JsonValue>>;
pub type ErrorReporter = Rc<dyn Fn(&Error)>;
pub type Kinds = Rc<RefCell<BTreeMap<String, Rc<ExecutableKind>>>>;
pub type Namespaces = Rc<RefCell<BTreeMap<String, NamespaceRegistration>>>;
/// Registered kind authority and executable callbacks. Tokens are compared by identity.
pub struct ExecutableKind {
    pub token: KindToken,
    pub turn: bool,
    pub config: KindConfig,
    pub slot: Option<SlotFactory>,
    pub describe: Option<TaskDescriber>,
    pub(crate) initial: super::scheduler::Handler,
    pub(crate) phases: BTreeMap<String, super::scheduler::Handler>,
    pub inflight: BTreeSet<String>,
    pub(crate) abort: super::scheduler::AbortHandler,
}
impl ExecutableKind { pub fn name(&self) -> &str { self.token.name() } }

#[derive(Clone, Debug, Default)]
pub struct CommitChanges {
    pub entries: Vec<Entry>, pub tasks: Vec<Task>, pub inputs: Vec<Input>,
    pub conversations: Vec<Conversation>, pub docs: Vec<(DocRef, Vec<maho_chord::delta::Op>)>,
    pub events: Vec<(Id, ViewEvent)>,
}
pub struct CommitResult<T> { pub value: T, pub seq: Option<Seq>, pub changes: CommitChanges }
#[derive(Default)]
pub struct CommitOptions { pub docs: Vec<DocRef>, pub closing: bool }
type Listener = Rc<dyn Fn(&CommitChanges) -> Result<()>>;
static LINE_KEY: LazyLock<maho_chord::types::ContextKey<bool>> = LazyLock::new(|| create_context_key("pico3.session.line"));
thread_local! { static OWNERS: RefCell<Vec<Weak<dyn Storage>>> = RefCell::new(Vec::new()); }

#[derive(Default)]
pub struct Defaults {
    pub rewindable: JsonObject, pub sticky: JsonObject,
    pub route: BTreeMap<String, DocKind>, owners: BTreeMap<String, KindToken>,
}
impl Defaults {
    pub fn register(&mut self, kind: &ExecutableKind) -> Result<()> {
        let mut seen = BTreeSet::new();
        for values in [&kind.config.rewindable, &kind.config.sticky] {
            for (key, _) in values.iter() {
                let key = key.as_str().ok_or_else(|| Error::InvalidShape("config key must be Unicode".into()))?;
                if !seen.insert(key.to_owned()) || self.route.contains_key(key) {
                    return Err(Error::InvalidShape(format!("config key {key} declared by more than one kind")));
                }
            }
        }
        for (doc, values) in [(DocKind::Rewindable, &kind.config.rewindable), (DocKind::Sticky, &kind.config.sticky)] {
            for (key, value) in values.iter() {
                let name = key.as_str().ok_or_else(|| Error::InvalidShape("config key must be Unicode".into()))?;
                self.route.insert(name.into(), doc);
                self.owners.insert(name.into(), kind.token.clone());
                match doc { DocKind::Rewindable => &mut self.rewindable, DocKind::Sticky => &mut self.sticky, DocKind::Session => unreachable!() }.insert(key.clone(), value.clone());
            }
        }
        Ok(())
    }
    pub fn unregister(&mut self, token: &KindToken) {
        let keys: Vec<_> = self.owners.iter().filter(|(_, owner)| owner.same_identity(token)).map(|(k, _)| k.clone()).collect();
        for key in keys {
            self.owners.remove(&key);
            match self.route.remove(&key) { Some(DocKind::Rewindable) => { self.rewindable.remove(&key); }, Some(DocKind::Sticky) => { self.sticky.remove(&key); }, Some(DocKind::Session) | None => {} }
        }
    }
    pub fn fill(&self, doc: DocKind, target: &mut JsonObject) {
        let defaults = match doc { DocKind::Rewindable => &self.rewindable, DocKind::Sticky => &self.sticky, DocKind::Session => return };
        for (key, value) in defaults.iter() { if target.get_key(key).is_none() { target.insert(key.clone(), value.clone()); } }
    }
    pub fn validate(&self, key: &str, value: &JsonValue) -> bool {
        let number = |v: &JsonValue| matches!(v, JsonValue::Number(n) if n.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0));
        let text = |v: &JsonValue| matches!(v, JsonValue::String(_));
        match key {
            "model" => match value { JsonValue::Object(v) => v.len() == 2 && ["provider", "modelId"].iter().all(|k| v.iter().find(|(name,_)| name == &JsString::from(*k)).is_some_and(|(_,v)| matches!(v,JsonValue::String(s) if s.as_str().is_some_and(|s| !s.is_empty())))), _ => false },
            "thinkingLevel" => matches!(value, JsonValue::String(v) if v.as_str().is_some_and(|v| ["off","minimal","low","medium","high","xhigh","max"].contains(&v))),
            "selectedTools" => matches!(value,JsonValue::Array(v) if v.iter().all(text)),
            "profile" => text(value),
            "threshold" => matches!(value,JsonValue::Number(n) if n.as_f64().is_some_and(f64::is_finite)),
            "keepRecent" => number(value),
            "steeringMode" | "followUpMode" => matches!(value,JsonValue::String(v) if v.as_str().is_some_and(|v| ["all","one-at-a-time"].contains(&v))),
            "retry" => match value {
                JsonValue::Object(v) => {
                    let get = |key: &str| v.iter().find(|(k,_)| k == &JsString::from(key)).map(|(_,v)|v);
                    (v.len() == 3 || v.len() == 4) && v.iter().all(|(k,_)| k.as_str().is_some_and(|k| ["enabled","maxRetries","baseDelayMs","maxAgentDelayMs"].contains(&k))) && matches!(get("enabled"),Some(JsonValue::Bool(_))) && matches!(get("maxRetries"),Some(JsonValue::Number(n)) if n.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991)) && get("baseDelayMs").is_some_and(number) && get("maxAgentDelayMs").is_none_or(number)
                }, _ => false,
            },
            _ => self.route.contains_key(key),
        }
    }
    fn seed(&self, doc: DocKind, seed: Option<JsonObject>) -> Result<JsonObject> {
        let mut out = JsonObject::new();
        for (key,value) in seed.unwrap_or_default().iter() {
            let key_text = key.as_str().ok_or_else(||Error::InvalidShape("invalid config key".into()))?;
            if self.route.get(key_text) != Some(&doc) || !self.validate(key_text,value) { return Err(Error::InvalidShape(format!("invalid config value for {key_text}"))); }
            out.insert(key.clone(),value.clone());
        }
        Ok(out)
    }
    fn fresh(&self, doc: DocKind, seed: Option<JsonObject>, preserve: bool) -> Result<JsonObject> {
        let mut base = JsonObject::new();
        base.insert("plugins".into(), JsonValue::object(Vec::new()));
        if doc == DocKind::Sticky {
            base.insert("inbox".into(),JsonValue::array(Vec::new()));
            base.insert("turn".into(),JsonValue::object(vec![("tools".into(),JsonValue::array(Vec::new()))]));
            base.insert("tasks".into(),JsonValue::object(Vec::new()));
        }
        self.fill(doc,&mut base);
        let mut raw = seed.unwrap_or_default();
        if !preserve { for key in ["plugins","inbox","turn","tasks"] { raw.remove(key); } raw = self.seed(doc,Some(raw))?; }
        for (key,value) in raw.into_inner() { base.insert(key,value); }
        Ok(base)
    }
}

pub struct Session {
    pub storage: Rc<dyn Storage>, pub kinds: Kinds, pub namespaces: Namespaces,
    pub defaults: RefCell<Defaults>, pub live_tasks: RefCell<BTreeMap<Id,Task>>,
    pub conversation_records: RefCell<BTreeMap<Id,Conversation>>, pub owner_task_cache: RefCell<BTreeMap<Id,Task>>,
    line: tokio::sync::Mutex<()>, closed: Cell<bool>, fault: RefCell<Option<String>>,
    now: Rc<dyn Fn() -> u64>, listeners: RefCell<Vec<Listener>>,
    line_listeners: RefCell<Vec<Listener>>, loaded_docs: RefCell<BTreeMap<DocRef,JsonObject>>,
    since_base: RefCell<BTreeMap<DocRef,usize>>,
    myself: Weak<Session>,
    pub on_report: RefCell<ErrorReporter>,
}
impl Session {
    pub fn new(storage: Rc<dyn Storage>, kinds: Kinds, namespaces: Namespaces, now: Rc<dyn Fn() -> u64>) -> Result<Rc<Self>> {
        let mut defaults = Defaults::default();
        for kind in kinds.borrow().values() { defaults.register(kind)?; }
        OWNERS.with(|owners| {
            let mut owners = owners.borrow_mut();
            owners.retain(|owner| owner.strong_count() != 0);
            if owners.iter().any(|owner| owner.upgrade().is_some_and(|owner|Rc::ptr_eq(&owner,&storage))) { return Err(Error::InvalidShape("Storage already has an owning Session".into())); }
            owners.push(Rc::downgrade(&storage)); Ok(())
        })?;
        Ok(Rc::new_cyclic(|myself|Self { storage,kinds,namespaces,defaults:RefCell::new(defaults),live_tasks:RefCell::new(BTreeMap::new()),conversation_records:RefCell::new(BTreeMap::new()),owner_task_cache:RefCell::new(BTreeMap::new()),line:tokio::sync::Mutex::new(()),closed:Cell::new(false),fault:RefCell::new(None),now,listeners:RefCell::new(Vec::new()),line_listeners:RefCell::new(Vec::new()),loaded_docs:RefCell::new(BTreeMap::new()),since_base:RefCell::new(BTreeMap::new()),myself:myself.clone(),on_report:RefCell::new(Rc::new(|error|tracing::error!(%error,"Pico3 post-commit failure"))) }))
    }
    pub async fn load(&self, ctx: &Context) -> Result<()> {
        self.on_line(ctx, |ctx| Box::pin(async move {
            let conversations = self.storage.conversations(&ctx).await?;
            let tasks = self.storage.scan_tasks(TaskScan::default(),&ctx).await?;
            if let Some(value)=self.storage.doc(DocRef::Session,&ctx).await?{self.loaded_docs.borrow_mut().insert(DocRef::Session,value);}
            for conversation in &conversations{for reference in [DocRef::Rewindable{conversation_id:conversation.id},DocRef::Sticky{conversation_id:conversation.id}]{if let Some(value)=self.storage.doc(reference.clone(),&ctx).await?{self.loaded_docs.borrow_mut().insert(reference,value);}}}
            *self.conversation_records.borrow_mut() = conversations.into_iter().map(|c|(c.id,c)).collect();
            for task in tasks { if task.status != TaskStatus::Terminal { self.live_tasks.borrow_mut().insert(task.id,task); } else if !task.owns.is_empty() { self.owner_task_cache.borrow_mut().insert(task.id,task); } }
            Ok(())
        })).await
    }
    fn usable(&self) -> Result<()> {
        if let Some(fault) = self.fault.borrow().as_ref() { return Err(Error::Faulted(fault.clone())); }
        if self.closed.get() { return Err(Error::Closed); } Ok(())
    }
    pub fn subscribe(&self, listener: Listener) { self.listeners.borrow_mut().push(listener); }
    pub fn subscribe_line(&self,listener:Listener){self.line_listeners.borrow_mut().push(listener);}
    pub fn loaded_document(&self,reference:&DocRef)->Result<JsonObject>{
        let mut value=self.loaded_docs.borrow().get(reference).cloned().ok_or_else(||Error::InvalidShape(format!("document {reference:?} is not cached")))?;
        self.defaults.borrow().fill(doc_kind(reference),&mut value);Ok(value)
    }
    pub fn now(&self)->u64{(self.now)()}
    pub fn register_kind(&self,kind:Rc<ExecutableKind>)->Result<()>{
        if self.kinds.borrow().contains_key(kind.name()){return Err(Error::InvalidShape(format!("kind {} registered twice",kind.name())));}
        self.defaults.borrow_mut().register(&kind)?;self.kinds.borrow_mut().insert(kind.name().into(),kind);Ok(())
    }
    pub fn unregister_kind(&self,token:&KindToken){
        if self.kinds.borrow().get(token.name()).is_some_and(|kind|kind.token.same_identity(token)){self.kinds.borrow_mut().remove(token.name());self.defaults.borrow_mut().unregister(token);}
    }
    pub async fn commit_host<T>(&self,conversation_id:Option<Id>,callback:impl for<'a> FnOnce(&'a mut dyn HostTx,Context)->LocalBoxFuture<'a,Result<T>>,ctx:&Context,options:CommitOptions)->Result<CommitResult<T>>{
        self.commit(Invoker::Host{conversation_id},|tx,ctx|callback(tx,ctx),ctx,options).await
    }
    pub fn kind_tokens(&self)->BTreeMap<String,KindToken>{self.kinds.borrow().iter().map(|(name,kind)|(name.clone(),kind.token.clone())).collect()}
    pub fn report(&self, error: &Error) { (self.on_report.borrow())(error); }
    pub async fn on_line<'a,T:'a>(&'a self, ctx: &Context, callback: impl FnOnce(Context) -> LocalBoxFuture<'a,Result<T>>) -> Result<T> {
        if ctx.value(&LINE_KEY) == Some(true) { return Err(Error::Forbidden("nested line operation".into())); }
        let _line = self.line.lock().await;
        self.usable()?; check_abort(ctx)?;
        callback(with_context_value(&LINE_KEY,true,ctx)).await
    }
    pub async fn read<'a,T:'a>(&'a self, ctx: &Context, callback: impl FnOnce(&'a dyn Storage, Context) -> LocalBoxFuture<'a,Result<T>>) -> Result<T> {
        self.on_line(ctx,|ctx|callback(self.storage.as_ref(),ctx)).await
    }
    pub(crate) async fn commit<T>(&self, invoker: Invoker, callback: impl for<'a> FnOnce(&'a mut TxImpl, Context) -> LocalBoxFuture<'a,Result<T>>, ctx: &Context, options: CommitOptions) -> Result<CommitResult<T>> {
        self.commit_scoped(invoker, async move |tx: &mut TxImpl, ctx: Context| callback(tx, ctx).await, ctx, options).await
    }
    /// The callback may borrow invocation data while lending the transaction to its future.
    pub(crate) async fn commit_scoped<T>(&self, invoker: Invoker, callback: impl for<'a> std::ops::AsyncFnOnce(&'a mut TxImpl, Context) -> Result<T>, ctx: &Context, options: CommitOptions) -> Result<CommitResult<T>> {
        let result = self.on_line(ctx,move|line_ctx|Box::pin(async move {
            if let Invoker::Task { token,id,mode,.. } = &invoker {
                token.check(*id,*mode)?;
                let live = self.live_tasks.borrow();
                let task = live.get(id).ok_or_else(||Error::Forbidden("commit from task that is not live".into()))?;
                if *mode == InvocationMode::Run && task.abort.is_some() { return Err(Error::Forbidden("commit from marked run invocation".into())); }
            }
            let mut tx = TxImpl::new(self.myself.upgrade().ok_or(Error::Closed)?,invoker,line_ctx.clone(),options.closing);
            let mut refs = options.docs;
            refs.push(DocRef::Session);
            if let Some(id) = tx.invocation_conversation() { refs.extend([DocRef::Rewindable{conversation_id:id},DocRef::Sticky{conversation_id:id}]); }
            tx.preload(&refs).await?;
            let value = callback(&mut tx,line_ctx.clone()).await?;
            check_abort(&line_ctx)?;
            let writes = tx.finish()?;
            let doc_sizes:Vec<_>=tx.changes.docs.iter().map(|(reference,ops)|Ok((reference.clone(),ops.first().is_some_and(|op|matches!(op,maho_chord::delta::Op::Replace(_))),serde_json::to_vec(ops)?.len()))).collect::<Result<_>>()?;
            tx.membrane.revoke();
            let seq = if writes.is_empty() && tx.changes.events.is_empty() { None } else {
                let persist_ctx = without_abort_signal(&line_ctx);
                match self.storage.commit(&writes,&persist_ctx).await {
                    Ok(seq) => Some(seq),
                    Err(error) => {
                        let fault = error.to_string(); *self.fault.borrow_mut() = Some(fault.clone());
                        if let Err(close_error) = self.storage.close(&persist_ctx).await { self.report(&close_error); }
                        return Err(Error::Faulted(fault));
                    }
                }
            };
            self.apply(&tx.changes);
            if seq.is_some(){
                for(reference,base,size)in doc_sizes{let mut sizes=self.since_base.borrow_mut();let current=sizes.entry(reference).or_default();if base{*current=0;}else{*current=current.saturating_add(size);}}
                for(reference,tracker)in tx.docs.borrow().iter(){if let JsonValue::Object(value)=tracker.target(){self.loaded_docs.borrow_mut().insert(reference.clone(),JsonObject::from((*value).clone()));}}
                let listeners=self.line_listeners.borrow().clone();for listener in listeners{if let Err(error)=listener(&tx.changes){self.report(&error);}}
            }
            Ok(CommitResult {value,seq,changes:tx.changes.clone()})
        })).await?;
        if result.seq.is_some() { let listeners = self.listeners.borrow().clone(); for listener in listeners { if let Err(error) = listener(&result.changes) { self.report(&error); } } }
        Ok(result)
    }
    fn apply(&self, changes: &CommitChanges) {
        for task in &changes.tasks {
            if task.status == TaskStatus::Terminal { self.live_tasks.borrow_mut().remove(&task.id); if !task.owns.is_empty() { self.owner_task_cache.borrow_mut().insert(task.id,task.clone()); } }
            else { self.live_tasks.borrow_mut().insert(task.id,task.clone()); }
        }
        for conversation in &changes.conversations { self.conversation_records.borrow_mut().insert(conversation.id,conversation.clone()); }
    }
    pub fn subtree(&self, root: Id) -> BTreeSet<Id> {
        let mut out = BTreeSet::from([root]);
        loop {
            let old = out.len();
            for conversation in self.conversation_records.borrow().values() {
                if let Some(owner) = conversation.owner { let task = self.live_tasks.borrow().get(&owner).cloned().or_else(||self.owner_task_cache.borrow().get(&owner).cloned()); if task.is_some_and(|t|out.contains(&t.conversation_id)) { out.insert(conversation.id); } }
            }
            if old == out.len() { return out; }
        }
    }
    pub fn ancestors(&self, id: Id) -> Vec<Id> {
        let mut chain = Vec::new(); let mut at = id; let mut seen = BTreeSet::new();
        while seen.insert(at) {
            let owner = self.conversation_records.borrow().get(&at).and_then(|c|c.owner);
            let Some(owner) = owner else { break };
            let task = self.live_tasks.borrow().get(&owner).cloned().or_else(||self.owner_task_cache.borrow().get(&owner).cloned());
            let Some(task) = task else { break }; at = task.conversation_id; chain.push(at);
        }
        chain.reverse(); chain
    }
    pub async fn fork(&self,parent_id:Id,at:Option<Id>,mut spec:ConversationSpec,ctx:&Context)->Result<Id> {
        let inherited = match at {
            None => None,
            Some(at) => self.read(ctx,|storage,ctx|Box::pin(async move {
                let before = at.checked_add(1).ok_or_else(||Error::InvalidShape("fork position overflow".into()))?;
                let rows = storage.scan_entries(EntryScan{conversation_id:parent_id,kind:None,with_head:None,before:Some(before),limit:1},&ctx).await?;
                if rows.first().is_none_or(|entry|entry.id != at) { return Err(Error::InvalidShape(format!("entry {at} is not visible from {parent_id}"))); }
                storage.doc_as_of(parent_id,at,&ctx).await
            })).await?,
        };
        let mut raw = spec.rewindable.take().unwrap_or_default(); raw.remove("plugins");
        let overrides = self.defaults.borrow().seed(DocKind::Rewindable,Some(raw))?;
        let mut rewindable = inherited.unwrap_or_default(); for (k,v) in overrides.into_inner() { rewindable.insert(k,v); }
        spec.rewindable = Some(rewindable); spec.parent = at.map(|at|ForkParent{conversation_id:parent_id,at:ForkAt::Entry(at)});
        Ok(self.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async move { tx.insert_conversation(spec,None,true) }),ctx,CommitOptions::default()).await?.value)
    }
    pub async fn retire(&self,task:&Task,ctx:&Context)->Result<()> {
        let id = task.id; let conversation_id = task.conversation_id;
        let idle = !self.live_tasks.borrow().values().any(|task|task.conversation_id == conversation_id);
        let compact=idle||self.since_base.borrow().get(&DocRef::Sticky{conversation_id}).copied().unwrap_or(0)>256*1024;
        self.commit(Invoker::Kernel{conversation_id:None},|tx,_|Box::pin(async move {
            let doc = tx.sticky(conversation_id)?; doc.at(&["tasks".into()])?.delete(id.to_string())?;
            if compact{tx.docs.borrow_mut().get_mut(&DocRef::Sticky{conversation_id}).ok_or_else(||Error::InvalidShape("sticky not loaded".into()))?.rebase();}Ok(())
        }),ctx,CommitOptions{docs:vec![DocRef::Sticky{conversation_id}],closing:false}).await?;
        if compact { self.on_line(ctx,|ctx|Box::pin(async move {self.storage.truncate(DocRef::Sticky{conversation_id},&without_abort_signal(&ctx)).await})).await?; } Ok(())
    }
    pub async fn close(&self,ctx:&Context)->Result<()> {
        if self.closed.get() { return Ok(()); }
        let _line = self.line.lock().await;
        self.storage.close(&without_abort_signal(ctx)).await?; self.closed.set(true);
        OWNERS.with(|owners|owners.borrow_mut().retain(|owner|owner.upgrade().is_some_and(|s| !Rc::ptr_eq(&s,&self.storage)))); Ok(())
    }
}

pub(crate) fn check_abort(ctx:&Context)->Result<()> {
    if ctx.is_aborted() { return Err(Error::Forbidden("operation aborted".into())); } Ok(())
}
impl Drop for Session{fn drop(&mut self){OWNERS.with(|owners|owners.borrow_mut().retain(|owner|owner.upgrade().is_some_and(|storage|!Rc::ptr_eq(&storage,&self.storage))));}}
fn encode<T:Serialize>(value:&T)->Result<JsonValue> { Ok(serde_json::from_str(&serde_json::to_string(value)?)?) }
fn decode<T:DeserializeOwned>(value:&JsonValue)->Result<T> { Ok(serde_json::from_str(&serde_json::to_string(value)?)?) }
fn doc_kind(reference:&DocRef)->DocKind { match reference {DocRef::Session=>DocKind::Session,DocRef::Sticky{..}=>DocKind::Sticky,DocRef::Rewindable{..}=>DocKind::Rewindable} }

pub struct TxImpl {
    session: Rc<Session>, invoker: Invoker, ctx: Context, pub closing: bool,
    writes: Vec<Write>, pub changes: CommitChanges,
    docs: RefCell<BTreeMap<DocRef,LegacyTracker>>, membrane: Membrane,
    plugin_views: RefCell<BTreeMap<String,LegacyTracker>>,
    tasks: BTreeMap<Id,Task>, entries: BTreeMap<Id,Entry>, conversations: BTreeMap<Id,Conversation>, inputs:BTreeMap<Id,Input>,
    wrote_entries:BTreeSet<Id>, wrote_tasks:bool, poisoned:RefCell<Option<(String,String)>>,
    changed_config:BTreeMap<Id,BTreeSet<String>>,
}
impl TxImpl {
    fn new(session:Rc<Session>,invoker:Invoker,ctx:Context,closing:bool)->Self {
        Self{session,invoker,ctx,closing,writes:Vec::new(),changes:CommitChanges::default(),docs:RefCell::new(BTreeMap::new()),membrane:Membrane::new("tx"),plugin_views:RefCell::new(BTreeMap::new()),tasks:BTreeMap::new(),entries:BTreeMap::new(),conversations:BTreeMap::new(),inputs:BTreeMap::new(),wrote_entries:BTreeSet::new(),wrote_tasks:false,poisoned:RefCell::new(None),changed_config:BTreeMap::new()}
    }
    fn core(&self)->bool { match &self.invoker {Invoker::Kernel{..}=>true,Invoker::Host{..}=>false,Invoker::Task{core,kind,..}=>*core && is_core_kind(kind.name())} }
    fn require_core(&self)->Result<()> { if self.core() {Ok(())} else {Err(Error::Forbidden("core turn machinery only".into()))} }
    fn invocation_conversation(&self)->Option<Id> { match &self.invoker {Invoker::Host{conversation_id}|Invoker::Kernel{conversation_id}=>*conversation_id,Invoker::Task{conversation_id,..}=>Some(*conversation_id)} }
    fn current_task(&self,id:Id)->Option<Task> {self.tasks.get(&id).cloned().or_else(||self.session.live_tasks.borrow().get(&id).cloned())}
    fn scope(&self,id:Id)->Result<()> {
        match &self.invoker {
            Invoker::Host{..}|Invoker::Kernel{..}=>Ok(()),
            Invoker::Task{id:task_id,conversation_id,..}=> {
                if self.core() || *conversation_id == id || self.conversations.contains_key(&id) || self.current_task(*task_id).is_some_and(|task|task.owns.iter().any(|root|self.session.subtree(*root).contains(&id))) {Ok(())} else {Err(Error::Forbidden(format!("conversation {id} outside task subtree")))}
            }
        }
    }
    fn entry_scope(&self,entry:&Entry)->Result<()> {
        if self.scope(entry.conversation_id).is_ok() {return Ok(());}
        let mut candidates = self.conversations.keys().copied().collect::<BTreeSet<_>>();
        if let Invoker::Task{id,conversation_id,..}=&self.invoker {candidates.insert(*conversation_id);if let Some(task)=self.current_task(*id) {for root in task.owns {candidates.extend(self.session.subtree(root));}}}
        for candidate in candidates {
            let mut at = self.conversations.get(&candidate).cloned().or_else(||self.session.conversation_records.borrow().get(&candidate).cloned());
            while let Some(parent)=at.and_then(|c|c.parent) {
                if parent.conversation_id == entry.conversation_id && entry.id <= parent.at {return Ok(());}
                at=self.session.conversation_records.borrow().get(&parent.conversation_id).cloned();
            }
        }
        Err(Error::Forbidden(format!("entry {} is not visible from task subtree",entry.id)))
    }
    fn scan_guard(&self,read:&str,entries:Option<Id>)->Result<()> {
        let violation = match entries {
            Some(id)=>{let mut at=Some(id);let mut seen=BTreeSet::new();let mut changed=false;while let Some(id)=at{if !seen.insert(id){break;}
                if self.wrote_entries.contains(&id){changed=true;break;}at=self.conversations.get(&id).cloned().or_else(||self.session.conversation_records.borrow().get(&id).cloned()).and_then(|conversation|conversation.parent).map(|parent|parent.conversation_id);}changed},
            None=>self.wrote_tasks
        };
        if violation {
            let write=if entries.is_some(){"entry append"}else{"task write"};
            if self.poisoned.borrow().is_none() {*self.poisoned.borrow_mut()=Some((read.into(),write.into()));}
            return Err(Error::ReadAfterWrite{read:read.into(),write:write.into()});
        } Ok(())
    }
    pub async fn preload(&self,refs:&[DocRef])->Result<()> {
        for reference in refs {
            if self.docs.borrow().contains_key(reference) {continue;}
            let mut value=match self.session.storage.doc(reference.clone(),&self.ctx).await?{Some(value)=>value,None=>{let mut value=JsonObject::new();value.insert("plugins".into(),JsonValue::object(Vec::new()));value}};
            self.session.defaults.borrow().fill(doc_kind(reference),&mut value);
            self.session.loaded_docs.borrow_mut().insert(reference.clone(),value.clone());
            let mut tracker=track(JsonValue::object(value.into_inner()))?; tracker.flush()?;
            self.docs.borrow_mut().insert(reference.clone(),tracker);
        } Ok(())
    }
    fn document(&self,reference:DocRef)->Result<Document> {
        let draft=self.docs.borrow_mut().get_mut(&reference).ok_or_else(||Error::InvalidShape(format!("document {reference:?} not preloaded")))?.state()?;
        self.membrane.wrap(draft)
    }
    fn raw_value(&self,reference:DocRef)->Result<JsonValue> {
        self.sync_plugins()?;
        Ok(self.docs.borrow_mut().get_mut(&reference).ok_or_else(||Error::InvalidShape("document not preloaded".into()))?.state()?.value()?)
    }
    fn sync_plugins(&self)->Result<()> {
        let conversation_id=self.invocation_conversation();
        for (name,tracker) in self.plugin_views.borrow_mut().iter_mut() {
            let registrations=self.session.namespaces.borrow();
            let registration=registrations.get(name).ok_or_else(||Error::Forbidden("namespace unregistered during transaction".into()))?;
            let values:JsonObject=decode(&tracker.state()?.value()?)?;
            if values.iter().any(|(key,_)|key.as_str().is_none_or(|key|!registration.routes.contains_key(key))){return Err(Error::Forbidden("undeclared namespace key".into()));}
            for (key,route) in &registration.routes {
                let reference=match route{DocKind::Session=>DocRef::Session,DocKind::Rewindable=>DocRef::Rewindable{conversation_id:conversation_id.ok_or_else(||Error::Forbidden("namespace requires conversation".into()))?},DocKind::Sticky=>DocRef::Sticky{conversation_id:conversation_id.ok_or_else(||Error::Forbidden("namespace requires conversation".into()))?}};
                let root=self.document(reference)?;
                let plugins=root.at(&["plugins".into()])?;
                if !plugins.has(name.clone())?{plugins.set(name.clone(),&JsonValue::object(Vec::new()))?;}
                let slice=plugins.at(&[name.clone().into()])?;
                match values.get(key){Some(value)=>slice.set(key.clone(),value)?,None=>slice.delete(key.clone())?,}
            }
        } Ok(())
    }
    fn seed_doc(&mut self,reference:DocRef,value:JsonObject)->Result<()> {
        let mut tracker=track(JsonValue::object(value.into_inner()))?; tracker.rebase();
        let ops=tracker.flush()?; self.writes.push(Write::Doc{r#ref:reference.clone(),ops:ops.clone()});self.changes.docs.push((reference.clone(),ops));self.docs.borrow_mut().insert(reference,tracker);Ok(())
    }
    fn finish(&mut self)->Result<Vec<Write>> {
        if let Some((read,write))=self.poisoned.borrow().as_ref() {return Err(Error::ReadAfterWrite{read:read.clone(),write:write.clone()});}
        self.sync_plugins()?;
        for (id,keys) in &self.changed_config {self.changes.events.push((*id,ViewEvent::Core(CoreViewEvent::ConfigChanged{keys:keys.iter().cloned().collect()})));}
        for (reference,tracker) in self.docs.borrow_mut().iter_mut() {
            let ops=tracker.flush()?;if !ops.is_empty(){self.writes.push(Write::Doc{r#ref:reference.clone(),ops:ops.clone()});self.changes.docs.push((reference.clone(),ops));}
        }
        Ok(std::mem::take(&mut self.writes))
    }
    pub(crate) fn set_task(&mut self,mut task:Task)->Result<()> {
        let previous=self.current_task(task.id);
        if task.status==TaskStatus::Terminal {task.checkpoint=None;let reference=DocRef::Sticky{conversation_id:task.conversation_id};if self.docs.borrow().contains_key(&reference){self.document(reference)?.at(&["tasks".into()])?.delete(task.id.to_string())?;}}
        self.writes.push(Write::TaskPatch{patch:TaskPatch{id:task.id,status:Some(task.status),checkpoint:Some(task.checkpoint.clone()),abort:task.abort,outcome:task.outcome.clone(),owns:Some(task.owns.clone())}});
        if task.status==TaskStatus::Terminal&&previous.is_some_and(|t|t.status!=TaskStatus::Terminal)&&self.session.kinds.borrow().get(&task.kind).is_none_or(|k|!k.turn)
            && let Some(outcome)=&task.outcome{let outcome=match outcome{Outcome::Completed{..}=>OutcomeStatus::Completed,Outcome::Failed{..}=>OutcomeStatus::Failed,Outcome::Aborted{..}=>OutcomeStatus::Aborted,Outcome::Orphaned=>OutcomeStatus::Orphaned,Outcome::Faulted{..}=>OutcomeStatus::Faulted};self.changes.events.push((task.conversation_id,ViewEvent::Core(CoreViewEvent::TaskEnded{task_id:task.id,kind:task.kind.clone(),outcome})));}
        self.tasks.insert(task.id,task.clone());self.changes.tasks.push(task);self.wrote_tasks=true;Ok(())
    }
    pub fn busy(&self,conversation_id:Id)->bool {
        let mut tasks=self.session.live_tasks.borrow().clone();tasks.extend(self.tasks.clone());
        tasks.values().any(|t|t.conversation_id==conversation_id && t.status!=TaskStatus::Terminal && t.background.is_none() && self.session.kinds.borrow().get(&t.kind).is_some_and(|k|k.turn) && !(self.closing && matches!(&self.invoker,Invoker::Task{id,..} if *id==t.id)))
    }
    fn append_internal(&mut self,conversation_id:Id,entry:NewEntry)->Result<Id> {
        if let Some(messages)=&entry.model{for message in messages{let value:JsonObject=decode(message.as_json())?;if !matches!(value.get("role"),Some(JsonValue::String(_))){return Err(Error::InvalidShape("malformed model message".into()));}}}
        let id=self.session.storage.mint_id()?;
        let head=entry.head.map(|h|match h{EntryHead::Id(id)=>id,EntryHead::Own(SelfHead::Own)=>id});
        if head.is_some_and(|head|head>id){return Err(Error::InvalidShape("head is in the future".into()));}
        let entry=Entry{id,conversation_id,kind:entry.kind,model:entry.model,data:entry.data,head,edits:entry.edits,by_task_id:match self.invoker{Invoker::Task{id,..}=>Some(id),Invoker::Host{..}|Invoker::Kernel{..}=>None}};
        self.writes.push(Write::Entry{entry:entry.clone()});self.changes.entries.push(entry.clone());
        if entry.head.is_some(){self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::HeadMoved{entry:entry.clone()})));}
        self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::EntryAdded{entry:entry.clone()})));self.entries.insert(id,entry);self.wrote_entries.insert(conversation_id);Ok(id)
    }
    fn put_input(&mut self,input:Input){self.writes.push(Write::Input{input:input.clone()});self.changes.inputs.push(input.clone());self.inputs.insert(input.id,input);}
    async fn set_input(&mut self,id:Id,resolution:InputResolution)->Result<()> {
        let mut input=self.input(id).await?.ok_or_else(||Error::InvalidShape(format!("input {id} not found")))?;
        match resolution{InputResolution::Done{answer}=>{input.status=InputStatus::Done;input.answer=Some(answer);},InputResolution::Unanswered{reason,detail}=>{input.status=InputStatus::Unanswered;input.reason=Some(reason);input.detail=detail;}}
        self.put_input(input);Ok(())
    }
    fn insert_conversation(&mut self,spec:ConversationSpec,owner:Option<Id>,preserve:bool)->Result<Id>{
        let id=self.session.storage.mint_id()?;
        let parent=spec.parent.and_then(|p|match p.at{ForkAt::Entry(at)=>Some(ConversationParent{conversation_id:p.conversation_id,at}),ForkAt::Start(Start::Start)=>None});
        let conversation=Conversation{id,parent,owner,sections:spec.sections};
        let rewindable=self.session.defaults.borrow().fresh(DocKind::Rewindable,spec.rewindable,preserve)?;
        let sticky=self.session.defaults.borrow().fresh(DocKind::Sticky,spec.sticky,false)?;
        self.writes.push(Write::Conversation{conversation:conversation.clone()});self.changes.conversations.push(conversation.clone());self.conversations.insert(id,conversation);
        self.seed_doc(DocRef::Rewindable{conversation_id:id},rewindable)?;self.seed_doc(DocRef::Sticky{conversation_id:id},sticky)?;
        if let Some(owner)=owner && let Some(mut task)=self.current_task(owner){task.owns.push(id);self.set_task(task)?;}
        Ok(id)
    }
}
impl Drop for TxImpl{fn drop(&mut self){self.membrane.revoke();}}
pub fn is_core_kind(name:&str)->bool{matches!(name,"pi.generation"|"pi.tool"|"pi.post_tools"|"pi.collapse")}

impl TxReads for TxImpl{
    fn conversation(&self,id:Id)->LocalBoxFuture<'_,Result<Option<Conversation>>>{Box::pin(async move{self.scope(id)?;match self.conversations.get(&id){Some(c)=>Ok(Some(c.clone())),None=>self.session.storage.conversation(id,&self.ctx).await}})}
    fn entry(&self,id:Id)->LocalBoxFuture<'_,Result<Option<Entry>>>{Box::pin(async move{let mut rows=self.entries(&[id]).await?;Ok(rows.remove(&id))})}
    fn entries<'a>(&'a self,ids:&'a [Id])->LocalBoxFuture<'a,Result<BTreeMap<Id,Entry>>>{Box::pin(async move{let mut out=self.session.storage.entries(ids,&self.ctx).await?;for id in ids{if let Some(entry)=self.entries.get(id){out.insert(*id,entry.clone());}}for entry in out.values(){self.entry_scope(entry)?;}Ok(out)})}
    fn newest_entry(&self,mut scan:EntryScan)->LocalBoxFuture<'_,Result<Option<Entry>>>{Box::pin(async move{scan.limit=1;Ok(self.scan_entries(scan).await?.into_iter().next())})}
    fn scan_entries(&self,scan:EntryScan)->LocalBoxFuture<'_,Result<Vec<Entry>>>{Box::pin(async move{self.scope(scan.conversation_id)?;self.scan_guard("scanEntries",Some(scan.conversation_id))?;self.session.storage.scan_entries(scan,&self.ctx).await})}
    fn context(&self,conversation_id:Id,at:Option<Id>)->LocalBoxFuture<'_,Result<ContextView>>{Box::pin(async move{self.scope(conversation_id)?;self.scan_guard("context",Some(conversation_id))?;derive_context(self.session.storage.as_ref(),conversation_id,at,&self.ctx).await})}
    fn task(&self,id:Id)->LocalBoxFuture<'_,Result<Option<Task>>>{Box::pin(async move{let task=match self.current_task(id){Some(task)=>Some(task),None=>self.session.storage.task(id,&self.ctx).await?};if let Some(task)=&task{self.scope(task.conversation_id)?;}Ok(task)})}
    fn tasks(&self,scan:TaskScan)->LocalBoxFuture<'_,Result<Vec<Task>>>{Box::pin(async move{self.scan_guard("tasks",None)?;if let Some(id)=scan.conversation_id{self.scope(id)?;}let mut out:BTreeMap<_,_>=self.session.storage.scan_tasks(scan.clone(),&self.ctx).await?.into_iter().map(|task|(task.id,task)).collect();out.extend(self.session.live_tasks.borrow().clone());out.into_values().filter(|t|scan.conversation_id.is_none_or(|id|id==t.conversation_id)&&scan.kind.as_ref().is_none_or(|k|k==&t.kind)&&scan.status.as_ref().is_none_or(|s|s.contains(&t.status))).map(|task|{self.scope(task.conversation_id)?;Ok(task)}).collect::<Result<Vec<_>>>()})}
    fn input(&self,id:Id)->LocalBoxFuture<'_,Result<Option<Input>>>{Box::pin(async move{let input=match self.inputs.get(&id){Some(input)=>Some(input.clone()),None=>self.session.storage.input(id,&self.ctx).await?};if let Some(input)=&input{self.scope(input.conversation_id)?;}Ok(input)})}
    fn rewindable_as_of(&self,conversation_id:Id,at:Id)->LocalBoxFuture<'_,Result<Option<RewindableState>>>{Box::pin(async move{self.scope(conversation_id)?;self.session.storage.doc_as_of(conversation_id,at,&self.ctx).await?.map(|object|decode(&JsonValue::object(object.into_inner()))).transpose()})}
    fn snapshot(&self,reference:DocRef)->Result<DocumentSnapshot>{
        if let DocRef::Rewindable{conversation_id}|DocRef::Sticky{conversation_id}=&reference{self.scope(*conversation_id)?;}
        let mut value:JsonObject=decode(&self.raw_value(reference.clone())?)?;self.session.defaults.borrow().fill(doc_kind(&reference),&mut value);let value=JsonValue::object(value.into_inner());
        match reference{DocRef::Session=>Ok(DocumentSnapshot::Session(decode(&value)?)),DocRef::Rewindable{..}=>Ok(DocumentSnapshot::Rewindable(decode(&value)?)),DocRef::Sticky{..}=>Ok(DocumentSnapshot::Sticky(decode(&value)?))}
    }
}

impl SharedTx for TxImpl{
    fn plugins(&self,namespace:&Namespace)->Result<Document>{
        let registrations=self.session.namespaces.borrow();
        let registration=registrations.get(namespace.id()).filter(|registration|registration.token.same_identity(namespace)&&namespace.is_registered()).ok_or_else(||Error::Forbidden("stale namespace token".into()))?;
        if !self.plugin_views.borrow().contains_key(namespace.id()){
            let mut merged=JsonObject::new();
            let conversation_id=self.invocation_conversation().ok_or_else(||Error::Forbidden("plugins require a conversation".into()))?;
            for (key,route) in &registration.routes{
                let (reference,defaults)=match route{DocKind::Session=>(DocRef::Session,&registration.defaults.session),DocKind::Rewindable=>(DocRef::Rewindable{conversation_id},&registration.defaults.rewindable),DocKind::Sticky=>(DocRef::Sticky{conversation_id},&registration.defaults.sticky)};
                let root=self.document(reference)?;let plugins=root.at(&["plugins".into()])?;
                let stored=match plugins.get(namespace.id())?{Some(slice)=>slice.get(key.clone())?.map(|value|value.value()).transpose()?,None=>None};
                if let Some(value)=stored.or_else(||defaults.get(key).cloned()){merged.insert(key.clone().into(),value);}
            }
            let mut tracker=track(JsonValue::object(merged.into_inner()))?;tracker.flush()?;
            self.plugin_views.borrow_mut().insert(namespace.id().into(),tracker);
        }
        let draft=self.plugin_views.borrow_mut().get_mut(namespace.id()).ok_or_else(||Error::InvalidShape("namespace view missing".into()))?.state()?;
        self.membrane.wrap(draft)
    }
    fn emit_plugin(&mut self,namespace:&Namespace,name:&str,data:JsonValue)->Result<()>{
        let id=self.invocation_conversation().ok_or_else(||Error::Forbidden("event requires conversation".into()))?;self.scope(id)?;
        if name.is_empty()||!name.bytes().enumerate().all(|(i,c)|if i==0{c.is_ascii_alphabetic()}else{c.is_ascii_alphanumeric()||matches!(c,b'_'|b'.'|b'-')}){return Err(Error::InvalidShape("invalid plugin event name".into()));}
        let registrations=self.session.namespaces.borrow();if registrations.get(namespace.id()).is_none_or(|r|!r.token.same_identity(namespace)){return Err(Error::Forbidden("stale namespace".into()));}
        self.changes.events.push((id,ViewEvent::plugin(namespace,name,data)?));Ok(())
    }
    fn config_get(&self,conversation_id:Id,key:&str)->Result<Option<JsonValue>>{
        self.scope(conversation_id)?;let defaults=self.session.defaults.borrow();
        let route=defaults.route.get(key).ok_or_else(||Error::InvalidShape(format!("unknown config key {key}")))?;
        let reference=match route{DocKind::Rewindable=>DocRef::Rewindable{conversation_id},DocKind::Sticky=>DocRef::Sticky{conversation_id},DocKind::Session=>DocRef::Session};
        let value:JsonObject=decode(&self.raw_value(reference)?)?;Ok(value.get(key).cloned().or_else(||match route{DocKind::Rewindable=>defaults.rewindable.get(key),DocKind::Sticky=>defaults.sticky.get(key),DocKind::Session=>None}.cloned()))
    }
    fn write(&mut self,conversation_id:Id,entry:NewEntry)->LocalBoxFuture<'_,Result<Id>>{Box::pin(async move{
        self.scope(conversation_id)?;
        if !self.core(){
            if entry.head.is_some()||entry.edits.is_some()||(entry.kind.starts_with("pi.")&&entry.kind!="pi.notice"){return Err(Error::Forbidden("reserved entry write".into()));}
            if entry.kind=="pi.notice" {
                let valid=entry.model.as_ref().is_some_and(|messages|messages.len()==1 && match messages[0].as_json(){JsonValue::Object(v)=>v.iter().any(|(key,value)|key==&JsString::from("role")&&matches!(value,JsonValue::String(s) if s.as_str()==Some("user"))),_=>false});
                if !valid{return Err(Error::Forbidden("pi.notice requires exactly one user message".into()));}
            }
        }
        let id=self.session.storage.mint_id()?;
        let mut input=Input{id,conversation_id,request_id:None,status:InputStatus::Queued,entry:None,answer:None,reason:None,detail:None};
        if self.busy(conversation_id){
            let inbox=self.document(DocRef::Sticky{conversation_id})?.at(&["inbox".into()])?;
            inbox.push(&[encode(&QueuedInput::Write{id,entry:to_stored(&entry)?})?])?;
            self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::InputQueued{input:id,mode:QueuedMode::Write})));
        }else{input.status=InputStatus::Done;input.entry=Some(self.append_internal(conversation_id,entry)?);}
        self.put_input(input);Ok(id)
    })}
    fn create_task(&mut self,kind:&KindToken,input:JsonValue,opts:TaskOptions)->Result<TaskRef>{
        if self.session.kinds.borrow().get(kind.name()).is_none_or(|k|!k.token.same_identity(kind)){return Err(Error::Forbidden("kind is not registered token".into()));}
        let id=self.create_task_internal(TaskSpec{kind:kind.name().into(),input,conversation_id:opts.conversation_id,after:Some(opts.after),background:opts.background})?;Ok(TaskRef{id,kind:kind.clone()})
    }
    fn create_conversation(&mut self,spec:ConversationSpec)->Result<Id>{
        if let Some(parent)=&spec.parent{self.scope(parent.conversation_id)?;}
        let owner=match self.invoker{Invoker::Task{id,..}=>Some(id),Invoker::Host{..}|Invoker::Kernel{..}=>None};self.insert_conversation(spec,owner,false)
    }
}
impl HostTx for TxImpl{
    fn config_set(&mut self,conversation_id:Id,key:&str,value:JsonValue)->Result<()>{
        self.scope(conversation_id)?;if matches!(self.invoker,Invoker::Task{..})&&!self.core(){return Err(Error::Forbidden("ordinary tasks cannot write config".into()));}
        let defaults=self.session.defaults.borrow();if !defaults.validate(key,&value){return Err(Error::InvalidShape(format!("invalid config value for {key}")));}
        let reference=match defaults.route.get(key){Some(DocKind::Rewindable)=>DocRef::Rewindable{conversation_id},Some(DocKind::Sticky)=>DocRef::Sticky{conversation_id},Some(DocKind::Session)=>DocRef::Session,None=>return Err(Error::InvalidShape(format!("unknown config key {key}")))};
        drop(defaults);
        self.document(reference)?.set(key,&value)?;self.changed_config.entry(conversation_id).or_default().insert(key.into());Ok(())
    }
    fn config_reset(&mut self,conversation_id:Id,key:&str)->Result<()>{
        self.scope(conversation_id)?;if matches!(self.invoker,Invoker::Task{..})&&!self.core(){return Err(Error::Forbidden("ordinary tasks cannot reset config".into()));}
        let route=self.session.defaults.borrow().route.get(key).copied().ok_or_else(||Error::InvalidShape(format!("unknown config key {key}")))?;
        let reference=match route{DocKind::Rewindable=>DocRef::Rewindable{conversation_id},DocKind::Sticky=>DocRef::Sticky{conversation_id},DocKind::Session=>DocRef::Session};
        self.document(reference)?.delete(key)?;self.changed_config.entry(conversation_id).or_default().insert(key.into());Ok(())
    }
}
impl TaskTx for TxImpl{
    fn checkpoint(&mut self,value:Checkpoint)->Result<()>{
        let id=match &self.invoker{Invoker::Task{id,mode:InvocationMode::Run,..}=>*id,Invoker::Task{mode:InvocationMode::Abort,..}=>return Err(Error::Forbidden("checkpoint from abort invocation".into())),Invoker::Host{..}|Invoker::Kernel{..}=>return Err(Error::Forbidden("checkpoint outside task".into()))};
        let mut task=self.current_task(id).ok_or_else(||Error::Forbidden("task is not live".into()))?;task.checkpoint=Some(value);self.set_task(task)
    }
    fn slot(&self,reference:&TaskRef)->Result<Document>{
        match &self.invoker{Invoker::Task{id,..} if *id==reference.id||self.core()=>{},Invoker::Task{..}|Invoker::Host{..}|Invoker::Kernel{..}=>return Err(Error::Forbidden("another task's slot or slot outside task".into()))}
        let task=self.current_task(reference.id).ok_or_else(||Error::InvalidShape("task is not live".into()))?;
        let kinds=self.session.kinds.borrow();let kind=kinds.get(&task.kind).filter(|kind|kind.token.same_identity(&reference.kind)).ok_or_else(||Error::Forbidden("stale kind token".into()))?;
        let slots=self.document(DocRef::Sticky{conversation_id:task.conversation_id})?.at(&["tasks".into()])?;let key=reference.id.to_string();
        if !slots.has(key.clone())?{let value=match &kind.slot{Some(initializer)=>initializer(&task.input)?,None=>JsonObject::new()};slots.set(key.clone(),&JsonValue::object(value.into_inner()))?;}
        slots.at(&[key.into()])
    }
}
impl TxImpl{
    fn create_task_internal(&mut self,spec:TaskSpec)->Result<Id>{
        let turn=self.session.kinds.borrow().get(&spec.kind).ok_or_else(||Error::InvalidShape(format!("unknown kind {}",spec.kind)))?.turn;
        if is_core_kind(&spec.kind)&&!self.core(){return Err(Error::Forbidden("create core task".into()));}
        let conversation_id=spec.conversation_id.or_else(||self.invocation_conversation()).ok_or_else(||Error::InvalidShape("conversationId required".into()))?;self.scope(conversation_id)?;
        let mut all=self.session.live_tasks.borrow().clone();all.extend(self.tasks.clone());
        if all.values().any(|t|t.conversation_id==conversation_id&&t.kind==spec.kind&&t.status!=TaskStatus::Terminal&&!(self.closing&&matches!(&self.invoker,Invoker::Task{id,..} if *id==t.id))){match spec.kind.as_str(){"pi.generation"=>return Err(Error::GenerationInProgress(conversation_id)),"pi.collapse"=>return Err(Error::CollapseInProgress(conversation_id)),_=>{}}}
        let id=self.session.storage.mint_id()?;
        let task=Task{id,conversation_id,kind:spec.kind,input:spec.input,status:TaskStatus::Pending,checkpoint:None,abort:None,outcome:None,after:spec.after.unwrap_or_default(),owns:Vec::new(),background:spec.background};
        self.writes.push(Write::Task{task:task.clone()});self.changes.tasks.push(task.clone());self.tasks.insert(id,task.clone());self.wrote_tasks=true;
        if !turn{self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::TaskStarted{task_id:id,kind:task.kind,background:task.background})));}Ok(id)
    }
    fn user_entry(&self,input:JsonValue)->Result<NewEntry>{
        let message=JsonValue::object(vec![("role".into(),JsonValue::String("user".into())),("content".into(),input),("timestamp".into(),JsonValue::Number((self.session.now)().into()))]);
        Ok(NewEntry{kind:"pi.user".into(),model:Some(vec![Stored::from_json(message)]),data:None,head:None,edits:None})
    }
    pub async fn withdraw_input(&mut self,id:Id)->Result<&'static str>{
        self.require_core()?;let Some(input)=self.input(id).await? else{return Ok("not_found")};if input.status!=InputStatus::Queued{return Ok("already_placed");}
        self.preload(&[DocRef::Sticky{conversation_id:input.conversation_id}]).await?;
        let inbox=self.document(DocRef::Sticky{conversation_id:input.conversation_id})?.at(&["inbox".into()])?;
        let values:Vec<QueuedInput>=decode(&inbox.value()?)?;for (index,item) in values.iter().enumerate().rev(){if queued_id(item)==id{inbox.splice(isize::try_from(index).map_err(|_|Error::InvalidShape("inbox too large".into()))?,1,&[])?;}}
        self.set_input(id,InputResolution::Unanswered{reason:InputReason::Aborted,detail:None}).await?;self.changes.events.push((input.conversation_id,ViewEvent::Core(CoreViewEvent::InputAborted{input:id})));Ok("aborted")
    }
}
fn queued_id(input:&QueuedInput)->Id{match input{QueuedInput::Steer{id,..}|QueuedInput::FollowUp{id,..}|QueuedInput::Write{id,..}=>*id}}
impl CoreTx for TxImpl{
    fn emit(&mut self,event:ViewEvent)->Result<()>{self.require_core()?;let id=self.invocation_conversation().ok_or_else(||Error::Forbidden("emit requires conversation".into()))?;self.changes.events.push((id,event));Ok(())}
    fn rewindable(&self,conversation_id:Id)->Result<Document>{self.require_core()?;self.document(DocRef::Rewindable{conversation_id})}
    fn sticky(&self,conversation_id:Id)->Result<Document>{self.require_core()?;self.document(DocRef::Sticky{conversation_id})}
    fn session(&self)->Result<Document>{self.require_core()?;self.document(DocRef::Session)}
    fn tool_slot(&self,conversation_id:Id,index:usize)->Result<Document>{self.require_core()?;self.document(DocRef::Sticky{conversation_id})?.at(&["turn".into(),"tools".into(),index.into()])}
    fn append_entry(&mut self,conversation_id:Id,entry:NewEntry)->Result<Id>{self.require_core()?;self.append_internal(conversation_id,entry)}
    fn create_task_spec(&mut self,spec:TaskSpec)->Result<Id>{self.require_core()?;self.create_task_internal(spec)}
    fn send(&mut self,conversation_id:Id,input:SendInput)->LocalBoxFuture<'_,Result<Id>>{Box::pin(async move{
        self.require_core()?;self.scope(conversation_id)?;
        if let Some(request_id)=&input.request_id{
            if let Some(existing)=self.inputs.values().find(|i|i.conversation_id==conversation_id&&i.request_id.as_ref()==Some(request_id)){return Ok(existing.id);}
            if let Some(existing)=self.session.storage.input_by_request(conversation_id,request_id,&self.ctx).await?{return Ok(existing.id);}
        }
        if self.busy(conversation_id){
            let mode=input.when_busy.unwrap_or(WhenBusy::FollowUp);if matches!(mode,WhenBusy::Reject){return Err(Error::ConversationBusy(conversation_id));}
            let id=self.session.storage.mint_id()?;let content=to_stored(&input.content)?;
            let (queued,mode)=match mode{WhenBusy::Steer=>(QueuedInput::Steer{id,input:content},QueuedMode::Steer),WhenBusy::FollowUp=>(QueuedInput::FollowUp{id,input:content},QueuedMode::FollowUp),WhenBusy::Reject=>unreachable!()};
            self.document(DocRef::Sticky{conversation_id})?.at(&["inbox".into()])?.push(&[encode(&queued)?])?;
            self.put_input(Input{id,conversation_id,request_id:input.request_id,status:InputStatus::Queued,entry:None,answer:None,reason:None,detail:None});
            self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::InputQueued{input:id,mode})));return Ok(id);
        }
        let head=self.newest_entry(EntryScan{conversation_id,kind:None,with_head:Some(true),before:None,limit:1}).await?;
        let mut triggers=self.boundary(conversation_id,Boundary::Final,head.map(|entry|entry.id)).await?.triggers;
        let id=self.session.storage.mint_id()?;let start=self.changes.events.len();let entry=self.append_internal(conversation_id,self.user_entry(encode(&input.content)?)?)?;let events=self.changes.events.split_off(start);
        self.put_input(Input{id,conversation_id,request_id:input.request_id,status:InputStatus::Placed,entry:Some(entry),answer:None,reason:None,detail:None});
        self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::InputPlaced{input:id,entry})));self.changes.events.extend(events);triggers.push(id);
        self.create_task_internal(TaskSpec{kind:"pi.generation".into(),input:JsonValue::object(vec![("inputs".into(),encode(&triggers)?)]),conversation_id:Some(conversation_id),after:None,background:None})?;
        self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::TurnStarted{inputs:triggers})));Ok(id)
    })}
    fn resolve_inputs<'a>(&'a mut self,ids:&'a [Id],resolution:InputResolution)->LocalBoxFuture<'a,Result<()>>{Box::pin(async move{self.require_core()?;for id in ids{self.set_input(*id,resolution.clone()).await?;}Ok(())})}
    fn boundary(&mut self,conversation_id:Id,at:Boundary,head_boundary:Option<Id>)->LocalBoxFuture<'_,Result<BoundaryResult>>{Box::pin(async move{
        self.require_core()?;
        let sticky=self.document(DocRef::Sticky{conversation_id})?;let state:StickyState=decode(&sticky.value()?)?;let mut inbox=state.inbox;inbox.sort_by_key(queued_id);
        let mut cut=None;for item in &inbox{if let QueuedInput::Write{id,entry}=item && matches!(entry.decode()?.head,Some(EntryHead::Own(SelfHead::Own))){cut=Some(*id);}}
        let mut selected=BTreeSet::new();let mut picked_steer=false;let mut picked_follow=false;let mut triggers=Vec::new();let mut head=head_boundary;
        for item in &inbox{
            let id=queued_id(item);
            if cut.is_some_and(|cut|id<cut)&&!matches!(item,QueuedInput::Write{..}){
                self.set_input(id,InputResolution::Unanswered{reason:InputReason::Stale,detail:None}).await?;selected.insert(id);self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::InputAborted{input:id})));continue;
            }
            let pick=match item{QueuedInput::Write{..}=>true,QueuedInput::Steer{..}=>{let pick=!picked_steer||matches!(state.steering_mode,QueueMode::All);picked_steer=true;pick},QueuedInput::FollowUp{..}=>{let pick=matches!(at,Boundary::Final)&&(!picked_follow||matches!(state.follow_up_mode,QueueMode::All));if pick{picked_follow=true;}pick}};
            if !pick{continue;}selected.insert(id);
            let (entry,passive)=match item{
                QueuedInput::Write{entry,..}=>{
                    let entry=entry.decode()?;
                    if matches!(entry.head,Some(EntryHead::Id(at)) if head.is_some_and(|head|at<head)){
                        self.set_input(id,InputResolution::Unanswered{reason:InputReason::Stale,detail:None}).await?;self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::InputAborted{input:id})));continue;
                    }(entry,true)
                },QueuedInput::Steer{input,..}|QueuedInput::FollowUp{input,..}=>(self.user_entry(input.as_json().clone())?,false),
            };
            let entry_head=entry.head.clone();let start=self.changes.events.len();let entry_id=self.append_internal(conversation_id,entry)?;let events=self.changes.events.split_off(start);
            if let Some(entry_head)=entry_head{head=Some(match entry_head{EntryHead::Id(id)=>id,EntryHead::Own(SelfHead::Own)=>entry_id});}
            let mut input=self.input(id).await?.ok_or_else(||Error::InvalidShape("queued input missing".into()))?;input.status=if passive{InputStatus::Done}else{InputStatus::Placed};input.entry=Some(entry_id);self.put_input(input);
            self.changes.events.push((conversation_id,ViewEvent::Core(CoreViewEvent::InputPlaced{input:id,entry:entry_id})));self.changes.events.extend(events);if !passive{triggers.push(id);}
        }
        let tracked=sticky.at(&["inbox".into()])?;for (index,item) in inbox.iter().enumerate().rev(){if selected.contains(&queued_id(item)){tracked.splice(isize::try_from(index).map_err(|_|Error::InvalidShape("inbox too large".into()))?,1,&[])?;}}
        Ok(BoundaryResult{triggers,terminated:cut.is_some()})
    })}
    fn create_owned_conversation(&mut self,owner_task_id:Id,source_conversation_id:Id,spec:OwnedConversationSpec)->LocalBoxFuture<'_,Result<Id>>{Box::pin(async move{
        self.require_core()?;let owner=self.current_task(owner_task_id).filter(|t|t.status!=TaskStatus::Terminal&&t.conversation_id==source_conversation_id).ok_or_else(||Error::Forbidden("task cannot create owned conversation".into()))?;
        let tip=if spec.inherit==Some(true){self.newest_entry(EntryScan{conversation_id:source_conversation_id,kind:None,with_head:None,before:None,limit:1}).await?}else{None};
        let inherited=match &tip{Some(tip)=>self.session.storage.doc_as_of(source_conversation_id,tip.id,&self.ctx).await?,None=>None};
        let preserve=inherited.is_some();let mut raw=spec.rewindable.unwrap_or_default();raw.remove("plugins");let overrides=self.session.defaults.borrow().seed(DocKind::Rewindable,Some(raw))?;let mut rewindable=inherited.unwrap_or_default();for(k,v)in overrides.into_inner(){rewindable.insert(k,v);}
        self.insert_conversation(ConversationSpec{parent:tip.map(|tip|ForkParent{conversation_id:source_conversation_id,at:ForkAt::Entry(tip.id)}),rewindable:Some(rewindable),sticky:spec.sticky,sections:None},Some(owner.id),preserve)
    })}
    fn create_fork_conversation(&mut self,spec:ConversationSpec)->Result<Id>{self.require_core()?;self.insert_conversation(spec,None,true)}
    fn mark_task(&mut self,id:Id)->Result<()>{self.require_core()?;let mut task=self.current_task(id).ok_or_else(||Error::InvalidShape("task not live".into()))?;task.abort=Some(True);self.set_task(task)}
}

#[cfg(test)]
#[path="session_tests.rs"]
mod tests;
