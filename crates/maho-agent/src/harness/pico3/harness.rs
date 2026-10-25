//! Experimental public Pico3 host; construction never activates the default harness.
pub use super::kinds::entries::entries;
pub use super::session::is_core_kind;
pub use super::view::{WATCH_CAPACITY, apply_envelope};
use super::{
    hooks::{HookRegistration, HookRunner, HookRunners},
    scheduler::{self, Hold, Scheduler, TxAccess, core_access, task_access},
    session::{CommitOptions, ExecutableKind, Session, TxImpl},
    system::{SectionRegistry, SystemSection, ToolRegistry},
    types::*,
    view::{self, ViewManager, Watch},
};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::{Rc, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub type Unregister = Box<dyn Fn()>;
pub type ConversationListener = Rc<dyn Fn(ConversationHandle) -> Result<()>>;
type ConversationListeners = RefCell<Vec<(Rc<()>, ConversationListener)>>;

pub struct HarnessOptions {
    pub models: Rc<dyn Models>,
    pub tools: Vec<Rc<dyn AnyToolDeclaration>>,
    pub sections: Vec<SystemSection>,
    pub plugins: BTreeMap<String, Rc<dyn PluginHandler>>,
    pub process_host: Option<Rc<dyn ProcessHost>>,
    pub now: Rc<dyn Fn() -> u64>,
    pub root: ConversationSpec,
    pub on_report: Rc<dyn Fn(&Error)>,
}
impl HarnessOptions {
    pub fn new(models: Rc<dyn Models>) -> Self {
        Self {
            models,
            tools: Vec::new(),
            sections: Vec::new(),
            plugins: BTreeMap::new(),
            process_host: None,
            now: Rc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                    .unwrap_or(0)
            }),
            root: ConversationSpec::default(),
            on_report: Rc::new(|error| tracing::error!(%error, "Pico3")),
        }
    }
}
pub struct BuiltinKinds {
    pub generation: KindToken,
    pub tool: KindToken,
    pub post_tools: KindToken,
    pub collapse: KindToken,
    pub job: KindToken,
    pub plugin: KindToken,
}
struct HookRecord {
    id: Rc<()>,
    namespace: Namespace,
    kind: KindToken,
    handlers: Rc<dyn Any>,
    conversation_id: Option<Id>,
    subtree: bool,
}
pub struct Harness {
    pub(crate) session: Rc<Session>,
    scheduler: Rc<Scheduler>,
    views: Rc<ViewManager>,
    sections: Rc<SectionRegistry>,
    tools: Rc<ToolRegistry>,
    options: HarnessOptions,
    hooks: RefCell<Vec<HookRecord>>,
    entry_kinds: RefCell<BTreeMap<String, Rc<()>>>,
    conversations: ConversationListeners,
    resumed: Cell<bool>,
    suspended: Cell<bool>,
    myself: Weak<Self>,
}
impl Harness {
    pub async fn open(
        storage: Rc<dyn Storage>,
        options: HarnessOptions,
        ctx: &Context,
    ) -> Result<Rc<Self>> {
        let sections = Rc::new(SectionRegistry::default());
        for section in super::system::system_sections()
            .into_values()
            .chain(options.sections.iter().cloned())
        {
            sections.register(section)?;
        }
        let tools = Rc::new(ToolRegistry::default());
        for tool in &options.tools {
            tools.register(tool.clone())?;
        }
        let session = Session::new(
            storage,
            Rc::new(RefCell::new(BTreeMap::new())),
            Rc::new(RefCell::new(BTreeMap::new())),
            options.now.clone(),
        )?;
        *session.on_report.borrow_mut() = options.on_report.clone();
        let views = ViewManager::new(session.clone(), options.on_report.clone());
        let scheduler = Scheduler::new(session.clone(), ctx.clone());
        let host = Rc::new_cyclic(|myself| Self {
            session: session.clone(),
            scheduler,
            views: views.clone(),
            sections,
            tools,
            options,
            hooks: RefCell::new(Vec::new()),
            entry_kinds: RefCell::new(BTreeMap::new()),
            conversations: RefCell::new(Vec::new()),
            resumed: Cell::new(false),
            suspended: Cell::new(false),
            myself: myself.clone(),
        });
        host.install(
            Rc::new(super::kinds::generation::generation(
                host.sections.clone(),
                host.tools.clone(),
            )?),
            &["prepared", "requesting", "retrying", "deferred"],
            core_access,
            true,
        )?;
        host.install(
            Rc::new(super::kinds::tool::tool()),
            &["started"],
            core_access,
            true,
        )?;
        host.install(
            Rc::new(super::kinds::post_tools::post_tools()?),
            &[],
            core_access,
            true,
        )?;
        host.install(
            Rc::new(super::kinds::collapse::collapse()?),
            &["summarizing", "retrying", "prepared"],
            core_access,
            true,
        )?;
        host.install(
            Rc::new(super::kinds::job::job()),
            &["waiting", "spawning", "running"],
            task_access,
            true,
        )?;
        host.install(
            Rc::new(super::kinds::plugin::plugin()),
            &["started"],
            task_access,
            true,
        )?;
        let weak = Rc::downgrade(&views);
        session.subscribe_line(Rc::new(move |changes| {
            if let Some(views) = weak.upgrade() {
                views.update(changes)?;
            }
            Ok(())
        }));
        let weak = Rc::downgrade(&host);
        session.subscribe(Rc::new(move |changes| {
            if let Some(host) = weak.upgrade() {
                host.views.deliver();
                let listeners: Vec<_> = host
                    .conversations
                    .borrow()
                    .iter()
                    .map(|(_, f)| f.clone())
                    .collect();
                for conversation in &changes.conversations {
                    for listener in &listeners {
                        host.notify(listener, host.handle(conversation.id));
                    }
                }
            }
            Ok(())
        }));
        session.load(ctx).await?;
        if session.conversation_records.borrow().is_empty() {
            host.create_conversation(host.options.root.clone(), None, ctx)
                .await?;
        }
        Ok(host)
    }
    fn install<K>(
        self: &Rc<Self>,
        kind: Rc<K>,
        phases: &[&str],
        access: TxAccess<K>,
        builtin: bool,
    ) -> Result<Rc<ExecutableKind>>
    where
        K: Kind + 'static,
        K::Input: 'static,
        K::Checkpoint: 'static,
        K::Result: 'static,
        K::Failure: 'static,
        K::Aborted: 'static,
        K::Hooks: 'static,
        K::Slot: 'static,
        K::Tx: 'static,
    {
        let name = kind.token().name().to_owned();
        if !builtin && name.starts_with("pi.") {
            return Err(Error::ReservedName {
                category: "task",
                name,
            });
        }
        if self.session.kinds.borrow().contains_key(&name) {
            return Err(Error::InvalidShape(format!(
                "task kind {name} already registered"
            )));
        }
        let weak = Rc::downgrade(self);
        let token = kind.token().clone();
        let runtime: scheduler::TypedRuntimeFactory<K> = Rc::new(move |task, lease, mode, _ctx| {
            let host = weak.upgrade().ok_or(Error::Closed)?;
            let runtime = HostRuntime::<K::Hooks, K::Tx>::new(
                host,
                task,
                token.clone(),
                lease,
                mode,
                access,
            )?;
            Ok(Rc::new(runtime))
        });
        let phases: Vec<_> = phases.iter().map(|p| (*p).to_owned()).collect();
        let executable = scheduler::executable(kind, &phases, runtime, access)?;
        self.session.defaults.borrow_mut().register(&executable)?;
        self.session
            .kinds
            .borrow_mut()
            .insert(name, executable.clone());
        self.scheduler.kick();
        Ok(executable)
    }
    pub fn register_task_kind<K>(
        self: &Rc<Self>,
        kind: Rc<K>,
        phases: &[&str],
        access: TxAccess<K>,
    ) -> Result<Unregister>
    where
        K: Kind + 'static,
        K::Input: 'static,
        K::Checkpoint: 'static,
        K::Result: 'static,
        K::Failure: 'static,
        K::Aborted: 'static,
        K::Hooks: 'static,
        K::Slot: 'static,
        K::Tx: 'static,
    {
        let registered = self.install(kind, phases, access, false)?;
        let weak = Rc::downgrade(self);
        Ok(Box::new(move || {
            if let Some(host) = weak.upgrade() {
                let current = host.session.kinds.borrow().get(registered.name()).cloned();
                if current.is_some_and(|k| Rc::ptr_eq(&k, &registered)) {
                    host.session.kinds.borrow_mut().remove(registered.name());
                    host.session
                        .defaults
                        .borrow_mut()
                        .unregister(&registered.token);
                }
            }
        }))
    }
    pub fn kinds(&self) -> Result<BuiltinKinds> {
        let kinds = self.session.kinds.borrow();
        let get = |name: &str| {
            kinds
                .get(name)
                .map(|k| k.token.clone())
                .ok_or_else(|| Error::InvalidShape(format!("missing builtin {name}")))
        };
        Ok(BuiltinKinds {
            generation: get("pi.generation")?,
            tool: get("pi.tool")?,
            post_tools: get("pi.post_tools")?,
            collapse: get("pi.collapse")?,
            job: get("pi.job")?,
            plugin: get("pi.plugin")?,
        })
    }
    pub fn register_tool(self: &Rc<Self>, tool: Rc<dyn AnyToolDeclaration>) -> Result<Unregister> {
        self.tools.register(tool.clone())?;
        let registry = self.tools.clone();
        Ok(Box::new(move || registry.unregister(&tool)))
    }
    pub fn register_section(self: &Rc<Self>, section: SystemSection) -> Result<Unregister> {
        self.sections.register(section.clone())?;
        let registry = self.sections.clone();
        Ok(Box::new(move || registry.unregister(&section)))
    }
    pub fn register_entry_kind<E>(&self, kind: &EntryKind<E>) -> Result<Unregister> {
        let name = kind.kind().to_owned();
        if name.starts_with("pi.") {
            return Err(Error::ReservedName {
                category: "entry",
                name,
            });
        }
        if self.entry_kinds.borrow().contains_key(&name) {
            return Err(Error::InvalidShape(format!(
                "entry kind {name} already registered"
            )));
        }
        let identity = Rc::new(());
        self.entry_kinds
            .borrow_mut()
            .insert(name.clone(), identity.clone());
        let weak = self.myself.clone();
        Ok(Box::new(move || {
            let Some(host) = weak.upgrade() else {
                return;
            };
            let remove = host
                .entry_kinds
                .borrow()
                .get(&name)
                .is_some_and(|id| Rc::ptr_eq(id, &identity));
            if remove {
                host.entry_kinds.borrow_mut().remove(&name);
            }
        }))
    }
    pub fn namespace(
        &self,
        id: &str,
        defaults: NamespaceDefaults,
        project: Option<NamespaceProjector>,
    ) -> Result<Namespace> {
        let mut chars = id.chars();
        if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            || !chars.all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
            || id.starts_with("pi.")
        {
            return Err(Error::Forbidden(format!("invalid namespace {id}")));
        }
        let mut registrations = self.session.namespaces.borrow_mut();
        if registrations
            .get(id)
            .is_some_and(|r| r.token.is_registered())
        {
            return Err(Error::Forbidden(format!(
                "namespace {id} already registered"
            )));
        }
        let mut routes = BTreeMap::new();
        for (doc, values) in [
            (DocKind::Rewindable, &defaults.rewindable),
            (DocKind::Sticky, &defaults.sticky),
            (DocKind::Session, &defaults.session),
        ] {
            for (key, _) in values.iter() {
                let key = key
                    .as_str()
                    .ok_or_else(|| Error::InvalidShape("namespace key must be Unicode".into()))?;
                if routes.insert(key.into(), doc).is_some() {
                    return Err(Error::InvalidShape(format!(
                        "namespace {id} key {key} declared twice"
                    )));
                }
            }
        }
        let token = Namespace::new(id);
        registrations.insert(
            id.into(),
            NamespaceRegistration {
                token: token.clone(),
                defaults,
                routes,
                project,
            },
        );
        Ok(token)
    }
    pub fn hooks<H: 'static>(
        &self,
        namespace: &Namespace,
        kind: &KindToken,
        handlers: Rc<H>,
        conversation_id: Option<Id>,
        subtree: bool,
    ) -> Result<Unregister> {
        if !namespace.is_registered()
            || !self
                .session
                .namespaces
                .borrow()
                .get(namespace.id())
                .is_some_and(|r| r.token.same_identity(namespace))
        {
            return Err(Error::Forbidden("namespace is stale".into()));
        }
        if !self
            .session
            .kinds
            .borrow()
            .get(kind.name())
            .is_some_and(|k| k.token.same_identity(kind))
        {
            return Err(Error::Forbidden("kind is not the registered token".into()));
        }
        let id = Rc::new(());
        self.hooks.borrow_mut().push(HookRecord {
            id: id.clone(),
            namespace: namespace.clone(),
            kind: kind.clone(),
            handlers,
            conversation_id,
            subtree,
        });
        let weak = self.myself.clone();
        Ok(Box::new(move || {
            if let Some(host) = weak.upgrade() {
                host.hooks.borrow_mut().retain(|h| !Rc::ptr_eq(&h.id, &id));
            }
        }))
    }
    pub async fn resume(&self, ctx: &Context) -> Result<()> {
        if self.suspended.get() {
            return Err(Error::Closed);
        }
        if self.resumed.replace(true) {
            return Ok(());
        }
        let orphaned: Vec<_> = self
            .session
            .live_tasks
            .borrow()
            .values()
            .filter(|t| !self.session.kinds.borrow().contains_key(&t.kind))
            .cloned()
            .collect();
        if !orphaned.is_empty() {
            let docs = orphaned
                .iter()
                .map(|t| DocRef::Sticky {
                    conversation_id: t.conversation_id,
                })
                .collect();
            self.session
                .commit(
                    Invoker::Kernel {
                        conversation_id: None,
                    },
                    |tx, _| {
                        Box::pin(async move {
                            for mut task in orphaned {
                                task.status = TaskStatus::Terminal;
                                task.outcome = Some(Outcome::Orphaned);
                                tx.set_task(task)?;
                            }
                            Ok(())
                        })
                    },
                    ctx,
                    CommitOptions {
                        docs,
                        closing: false,
                    },
                )
                .await?;
        }
        if !self.suspended.get() {
            self.scheduler.resume();
        }
        Ok(())
    }
    pub fn quiescent(&self) -> bool {
        self.scheduler.quiescent()
    }
    pub fn hold(&self) -> Result<Hold> {
        if !self.quiescent() {
            return Err(Error::Forbidden("cannot hold non-quiescent harness".into()));
        }
        Ok(self.scheduler.hold())
    }
    pub async fn suspend(&self, ctx: &Context) -> Result<()> {
        if self.suspended.replace(true) {
            return Ok(());
        }
        let result = async {
            self.scheduler.join_all().await?;
            let tools: Vec<_> = self
                .session
                .live_tasks
                .borrow()
                .values()
                .filter(|t| t.kind == "pi.tool")
                .cloned()
                .collect();
            if tools.is_empty() {
                return Ok(());
            }
            let docs = tools
                .iter()
                .map(|t| DocRef::Sticky {
                    conversation_id: t.conversation_id,
                })
                .collect();
            self.session
                .commit(
                    Invoker::Kernel {
                        conversation_id: None,
                    },
                    |tx, _| {
                        Box::pin(async move {
                            for task in tools {
                                #[derive(serde::Deserialize)]
                                struct Tool {
                                    index: usize,
                                }
                                let input: Tool = view::decode(&task.input)?;
                                let slots = tx
                                    .sticky(task.conversation_id)?
                                    .at(&["turn".into(), "tools".into()])?;
                                if let Some(slot) = slots.get(input.index)? {
                                    slot.delete("waitingOn")?;
                                }
                            }
                            Ok(())
                        })
                    },
                    ctx,
                    CommitOptions {
                        docs,
                        closing: false,
                    },
                )
                .await?;
            Ok(())
        }
        .await;
        self.views.close();
        let closed = self.session.close(ctx).await;
        result.and(closed)
    }
    pub async fn close(&self, ctx: &Context) -> Result<()> {
        self.suspend(ctx).await
    }
    fn handle(&self, id: Id) -> ConversationHandle {
        ConversationHandle {
            id,
            host: self.myself.clone(),
        }
    }
    pub async fn root(&self, ctx: &Context) -> Result<ConversationHandle> {
        self.conversation(1, ctx)
            .await?
            .ok_or_else(|| Error::InvalidShape("root conversation missing".into()))
    }
    pub async fn conversation(&self, id: Id, ctx: &Context) -> Result<Option<ConversationHandle>> {
        self.session
            .read(ctx, |s, c| {
                Box::pin(async move { s.conversation(id, &c).await })
            })
            .await
            .map(|c| c.map(|_| self.handle(id)))
    }
    pub async fn create_conversation(
        &self,
        spec: ConversationSpec,
        input: Option<UserInput>,
        ctx: &Context,
    ) -> Result<ConversationHandle> {
        let id = self
            .session
            .commit(
                Invoker::Kernel {
                    conversation_id: None,
                },
                |tx, _| {
                    Box::pin(async move {
                        let id = tx.create_conversation(spec)?;
                        if let Some(content) = input {
                            tx.send(
                                id,
                                SendInput {
                                    content,
                                    request_id: None,
                                    when_busy: None,
                                },
                            )
                            .await?;
                        }
                        Ok(id)
                    })
                },
                ctx,
                CommitOptions::default(),
            )
            .await?
            .value;
        Ok(self.handle(id))
    }
    pub fn on_conversation(
        &self,
        listener: Rc<dyn Fn(ConversationHandle) -> Result<()>>,
    ) -> Unregister {
        let id = Rc::new(());
        self.conversations
            .borrow_mut()
            .push((id.clone(), listener.clone()));
        let ids: Vec<_> = self
            .session
            .conversation_records
            .borrow()
            .keys()
            .copied()
            .collect();
        for id in ids {
            self.notify(&listener, self.handle(id));
        }
        let weak = self.myself.clone();
        Box::new(move || {
            if let Some(host) = weak.upgrade() {
                host.conversations
                    .borrow_mut()
                    .retain(|(key, _)| !Rc::ptr_eq(key, &id));
            }
        })
    }
    fn notify(
        &self,
        listener: &Rc<dyn Fn(ConversationHandle) -> Result<()>>,
        handle: ConversationHandle,
    ) {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(handle))) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => view::report(&self.options.on_report, &error),
            Err(_) => view::report(
                &self.options.on_report,
                &Error::InvalidShape("conversation listener panicked".into()),
            ),
        }
    }
    pub async fn entries(&self, scan: EntryScan, ctx: &Context) -> Result<Vec<Entry>> {
        self.session
            .read(ctx, |s, c| {
                Box::pin(async move { s.scan_entries(scan, &c).await })
            })
            .await
    }
    pub async fn get_task(&self, id: Id, ctx: &Context) -> Result<Option<Task>> {
        self.session
            .read(ctx, |s, c| Box::pin(async move { s.task(id, &c).await }))
            .await
    }
    pub async fn abort_input(
        &self,
        id: Id,
        scope: Option<Id>,
        ctx: &Context,
    ) -> Result<&'static str> {
        self.session
            .commit(
                Invoker::Kernel {
                    conversation_id: None,
                },
                |tx, _| {
                    Box::pin(async move {
                        if let Some(scope) = scope
                            && tx
                                .input(id)
                                .await?
                                .is_some_and(|i| i.conversation_id != scope)
                        {
                            return Err(Error::Forbidden(format!(
                                "input {id} outside conversation {scope}"
                            )));
                        }
                        tx.withdraw_input(id).await
                    })
                },
                ctx,
                CommitOptions::default(),
            )
            .await
            .map(|r| r.value)
    }
    pub async fn abort_task(&self, id: Id, ctx: &Context) -> Result<AbortTaskResult> {
        self.scheduler.abort_task(id, ctx).await
    }
    pub async fn mark_task(&self, id: Id, ctx: &Context) -> Result<AbortTaskResult> {
        self.session
            .commit(
                Invoker::Kernel {
                    conversation_id: None,
                },
                |tx, _| {
                    Box::pin(async move {
                        let task = tx
                            .task(id)
                            .await?
                            .ok_or_else(|| Error::InvalidShape(format!("task {id} missing")))?;
                        if task.status == TaskStatus::Terminal {
                            return Ok(AbortTaskResult::Terminal);
                        }
                        tx.mark_task(id)?;
                        Ok(AbortTaskResult::Marked)
                    })
                },
                ctx,
                CommitOptions::default(),
            )
            .await
            .map(|r| r.value)
    }
    pub async fn wait_for_idle(&self, ctx: &Context) -> Result<()> {
        self.scheduler.wait_for_idle(None, ctx).await
    }
    pub async fn wait_for_task(&self, id: Id, ctx: &Context) -> Result<Task> {
        self.scheduler.wait_for_task(id, ctx).await
    }
    fn assert_invocation(
        &self,
        task: Id,
        token: &InvocationToken,
        mode: InvocationMode,
    ) -> Result<()> {
        token.check(task, mode)?;
        let tasks = self.session.live_tasks.borrow();
        let task = tasks
            .get(&task)
            .ok_or_else(|| Error::Forbidden("invocation task is not live".into()))?;
        if mode == InvocationMode::Run && task.abort.is_some() {
            return Err(Error::Forbidden("run invocation marked".into()));
        }
        Ok(())
    }
    fn assert_owned(&self, task: Id, conversation: Id, allow_source: bool) -> Result<()> {
        let tasks = self.session.live_tasks.borrow();
        if tasks.get(&task).is_some_and(|t| {
            (allow_source && t.conversation_id == conversation)
                || t.owns
                    .iter()
                    .any(|root| self.session.subtree(*root).contains(&conversation))
        }) {
            return Ok(());
        }
        Err(Error::Forbidden(format!(
            "conversation {conversation} outside task {task} owned subtree"
        )))
    }
}

#[derive(Clone)]
pub struct ConversationHandle {
    pub id: Id,
    host: Weak<Harness>,
}
impl ConversationHandle {
    fn host(&self) -> Result<Rc<Harness>> {
        self.host.upgrade().ok_or(Error::Closed)
    }
    fn docs(&self) -> Vec<DocRef> {
        vec![
            DocRef::Rewindable {
                conversation_id: self.id,
            },
            DocRef::Sticky {
                conversation_id: self.id,
            },
        ]
    }
    pub async fn commit<T>(
        &self,
        callback: impl for<'a> FnOnce(&'a mut dyn HostTx, Context) -> LocalBoxFuture<'a, Result<T>>,
        ctx: &Context,
    ) -> Result<T> {
        self.host()?
            .session
            .commit(
                Invoker::Host {
                    conversation_id: Some(self.id),
                },
                |tx, c| callback(tx, c),
                ctx,
                CommitOptions {
                    docs: self.docs(),
                    closing: false,
                },
            )
            .await
            .map(|r| r.value)
    }
    pub async fn config_get(&self, ctx: &Context) -> Result<JsonObject> {
        let host = self.host()?;
        let keys: Vec<_> = host
            .session
            .defaults
            .borrow()
            .route
            .keys()
            .cloned()
            .collect();
        let id = self.id;
        self.commit(
            move |tx, _| {
                Box::pin(async move {
                    let mut out = JsonObject::new();
                    for key in keys {
                        if let Some(value) = tx.config_get(id, &key)? {
                            out.insert(key.into(), value);
                        }
                    }
                    Ok(out)
                })
            },
            ctx,
        )
        .await
    }
    pub async fn config_set(&self, patch: JsonObject, ctx: &Context) -> Result<()> {
        let id = self.id;
        self.commit(
            move |tx, _| {
                Box::pin(async move {
                    for (key, value) in patch.into_inner() {
                        tx.config_set(
                            id,
                            key.as_str()
                                .ok_or_else(|| Error::InvalidShape("invalid config key".into()))?,
                            value,
                        )?;
                    }
                    Ok(())
                })
            },
            ctx,
        )
        .await
    }
    pub async fn config_reset(&self, keys: Vec<String>, ctx: &Context) -> Result<()> {
        let id = self.id;
        self.commit(
            move |tx, _| {
                Box::pin(async move {
                    for key in keys {
                        tx.config_reset(id, &key)?;
                    }
                    Ok(())
                })
            },
            ctx,
        )
        .await
    }
    pub async fn send(&self, input: SendInput, ctx: &Context) -> Result<InputHandle> {
        let host = self.host()?;
        let id = self.id;
        let input = host
            .session
            .commit(
                Invoker::Kernel {
                    conversation_id: Some(id),
                },
                |tx, _| Box::pin(async move { tx.send(id, input).await }),
                ctx,
                CommitOptions {
                    docs: self.docs(),
                    closing: false,
                },
            )
            .await?
            .value;
        Ok(InputHandle {
            id: input,
            host: self.host.clone(),
        })
    }
    pub async fn write(&self, entry: NewEntry, ctx: &Context) -> Result<Id> {
        let id = self.id;
        self.commit(
            move |tx, _| Box::pin(async move { tx.write(id, entry).await }),
            ctx,
        )
        .await
    }
    pub async fn rewindable(&self, ctx: &Context) -> Result<RewindableState> {
        let id = self.id;
        self.commit(
            move |tx, _| {
                Box::pin(async move {
                    match tx.snapshot(DocRef::Rewindable {
                        conversation_id: id,
                    })? {
                        DocumentSnapshot::Rewindable(s) => Ok(s),
                        _ => Err(Error::InvalidShape("wrong document".into())),
                    }
                })
            },
            ctx,
        )
        .await
    }
    pub async fn sticky(&self, ctx: &Context) -> Result<StickyState> {
        let id = self.id;
        self.commit(
            move |tx, _| {
                Box::pin(async move {
                    match tx.snapshot(DocRef::Sticky {
                        conversation_id: id,
                    })? {
                        DocumentSnapshot::Sticky(s) => Ok(s),
                        _ => Err(Error::InvalidShape("wrong document".into())),
                    }
                })
            },
            ctx,
        )
        .await
    }
    pub async fn context(&self, ctx: &Context) -> Result<ContextView> {
        let id = self.id;
        self.commit(
            move |tx, _| Box::pin(async move { tx.context(id, None).await }),
            ctx,
        )
        .await
    }
    pub async fn fork(&self, at: ForkAt, spec: ConversationSpec, ctx: &Context) -> Result<Self> {
        let host = self.host()?;
        let at = match at {
            ForkAt::Entry(id) => Some(id),
            ForkAt::Start(Start::Start) => None,
        };
        let id = host.session.fork(self.id, at, spec, ctx).await?;
        Ok(host.handle(id))
    }
    pub async fn collapse(&self, instructions: Option<String>, ctx: &Context) -> Result<Id> {
        let host = self.host()?;
        let id = self.id;
        host.session
            .commit(
                Invoker::Kernel {
                    conversation_id: Some(id),
                },
                |tx, _| {
                    Box::pin(async move {
                        let entries = tx.context(id, None).await?.entries;
                        let DocumentSnapshot::Rewindable(state) =
                            tx.snapshot(DocRef::Rewindable {
                                conversation_id: id,
                            })?
                        else {
                            return Err(Error::InvalidShape("wrong rewindable document".into()));
                        };
                        let through =
                            super::kinds::collapse::choose_through(&entries, state.keep_recent)?
                                .ok_or_else(|| Error::InvalidShape("nothing to collapse".into()))?;
                        let mut input = JsonObject::new();
                        input.insert("reason".into(), JsonValue::String("manual".into()));
                        input.insert("through".into(), view::encode(&through)?);
                        if let Some(instructions) = instructions {
                            input.insert(
                                "instructions".into(),
                                JsonValue::String(instructions.into()),
                            );
                        }
                        tx.create_task_spec(TaskSpec {
                            kind: "pi.collapse".into(),
                            input: JsonValue::object(input.into_inner()),
                            conversation_id: Some(id),
                            background: Some(True),
                            after: None,
                        })
                    })
                },
                ctx,
                CommitOptions {
                    docs: self.docs(),
                    closing: false,
                },
            )
            .await
            .map(|r| r.value)
    }
    pub async fn reset(&self, handoff: Option<String>, ctx: &Context) -> Result<()> {
        let host = self.host()?;
        let timestamp = (host.options.now)();
        let id = self.id;
        host.session
            .commit(
                Invoker::Kernel {
                    conversation_id: Some(id),
                },
                |tx, _| {
                    Box::pin(async move {
                        let (kind, model) = match handoff {
                            None => ("pi.reset", None),
                            Some(content) => {
                                let value = JsonValue::object(vec![
                                    ("role".into(), JsonValue::String("user".into())),
                                    ("content".into(), JsonValue::String(content.into())),
                                    ("timestamp".into(), view::encode(&timestamp)?),
                                ]);
                                ("pi.handoff", Some(vec![Stored::from_json(value)]))
                            }
                        };
                        tx.write(
                            id,
                            NewEntry {
                                kind: kind.into(),
                                model,
                                data: None,
                                head: Some(EntryHead::Own(SelfHead::Own)),
                                edits: None,
                            },
                        )
                        .await?;
                        Ok(())
                    })
                },
                ctx,
                CommitOptions {
                    docs: self.docs(),
                    closing: false,
                },
            )
            .await
            .map(|r| r.value)
    }
    pub async fn abort(&self, ctx: &Context) -> Result<()> {
        let host = self.host()?;
        let id = self.id;
        let source = host.clone();
        let (marked, owned) = host
            .session
            .commit(
                Invoker::Kernel {
                    conversation_id: Some(id),
                },
                |tx, _| {
                    Box::pin(async move {
                        let DocumentSnapshot::Sticky(state) = tx.snapshot(DocRef::Sticky {
                            conversation_id: id,
                        })?
                        else {
                            return Err(Error::InvalidShape("wrong sticky document".into()));
                        };
                        let withdrawn: Vec<_> = state
                            .inbox
                            .iter()
                            .filter_map(|q| match q {
                                QueuedInput::Steer { id, .. }
                                | QueuedInput::FollowUp { id, .. } => Some(*id),
                                QueuedInput::Write { .. } => None,
                            })
                            .collect();
                        let keep: Vec<_> = state
                            .inbox
                            .into_iter()
                            .filter(|q| matches!(q, QueuedInput::Write { .. }))
                            .collect();
                        tx.sticky(id)?.set("inbox", &view::encode(&keep)?)?;
                        tx.resolve_inputs(
                            &withdrawn,
                            InputResolution::Unanswered {
                                reason: InputReason::Aborted,
                                detail: None,
                            },
                        )
                        .await?;
                        for input in withdrawn {
                            tx.emit(ViewEvent::Core(CoreViewEvent::InputAborted { input }))?;
                        }
                        let tasks: Vec<_> = source
                            .session
                            .live_tasks
                            .borrow()
                            .values()
                            .filter(|t| t.conversation_id == id && t.background.is_none())
                            .cloned()
                            .collect();
                        let mut marked = Vec::new();
                        let mut owned = Vec::new();
                        for task in tasks {
                            owned.extend(task.owns);
                            if task.abort.is_none() {
                                tx.mark_task(task.id)?;
                            }
                            marked.push(task.id);
                        }
                        Ok((marked, owned))
                    })
                },
                ctx,
                CommitOptions {
                    docs: self.docs(),
                    closing: false,
                },
            )
            .await?
            .value;
        for task in marked {
            host.scheduler.abort_task(task, ctx).await?;
        }
        host.scheduler.wait_for_idle(Some(id), ctx).await?;
        for id in owned {
            host.scheduler.wait_for_idle(Some(id), ctx).await?;
        }
        Ok(())
    }
    pub async fn wait_for_idle(&self, ctx: &Context) -> Result<()> {
        self.host()?
            .scheduler
            .wait_for_idle(Some(self.id), ctx)
            .await
    }
    pub fn hooks<H: 'static>(
        &self,
        namespace: &Namespace,
        kind: &KindToken,
        handlers: Rc<H>,
        subtree: bool,
    ) -> Result<Unregister> {
        self.host()?
            .hooks(namespace, kind, handlers, Some(self.id), subtree)
    }
    pub async fn watch(&self, ctx: &Context) -> Result<Watch> {
        let host = self.host()?;
        let views = host.views.clone();
        let id = self.id;
        host.session
            .commit(
                Invoker::Host {
                    conversation_id: Some(id),
                },
                |tx, _| {
                    Box::pin(async move {
                        let conversation = tx
                            .conversation(id)
                            .await?
                            .ok_or_else(|| Error::InvalidShape("conversation missing".into()))?;
                        let entries = capture_active_transcript(tx, id).await?;
                        views.watch(conversation, entries)
                    })
                },
                ctx,
                CommitOptions {
                    docs: self.docs(),
                    closing: false,
                },
            )
            .await
            .map(|r| r.value)
    }
}
#[derive(Clone)]
pub struct InputHandle {
    pub id: Id,
    host: Weak<Harness>,
}
impl InputHandle {
    pub async fn result(&self, ctx: &Context) -> Result<Option<Input>> {
        let host = self.host.upgrade().ok_or(Error::Closed)?;
        let id = self.id;
        host.session
            .read(ctx, |s, c| Box::pin(async move { s.input(id, &c).await }))
            .await
    }
    pub async fn wait(&self, ctx: &Context) -> Result<Input> {
        self.host
            .upgrade()
            .ok_or(Error::Closed)?
            .scheduler
            .wait_for_input(self.id, ctx)
            .await
    }
    pub async fn abort(&self, ctx: &Context) -> Result<&'static str> {
        self.host
            .upgrade()
            .ok_or(Error::Closed)?
            .abort_input(self.id, None, ctx)
            .await
    }
}
impl super::types::InputHandle for InputHandle {
    fn id(&self) -> Id {
        self.id
    }
    fn result<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Option<Input>>> {
        Box::pin(self.result(ctx))
    }
    fn wait<'a>(&'a self, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Input>> {
        Box::pin(self.wait(ctx))
    }
}
pub async fn capture_active_transcript(
    tx: &dyn TxReads,
    conversation_id: Id,
) -> Result<Vec<Entry>> {
    let head = tx
        .scan_entries(EntryScan {
            conversation_id,
            kind: None,
            with_head: Some(true),
            before: None,
            limit: 1,
        })
        .await?
        .first()
        .and_then(|e| e.head);
    let mut before = None;
    let mut entries = Vec::new();
    loop {
        let page = tx
            .scan_entries(EntryScan {
                conversation_id,
                kind: None,
                with_head: None,
                before,
                limit: 256,
            })
            .await?;
        let mut done = page.len() < 256;
        before = page.last().map(|e| e.id);
        for entry in page {
            if head.is_some_and(|head| entry.id < head) {
                done = true;
                break;
            }
            entries.push(entry);
        }
        if done {
            break;
        }
    }
    entries.reverse();
    Ok(entries)
}

struct HostRuntime<H, Tx: TaskTx + ?Sized> {
    host: Rc<Harness>,
    task: Task,
    kind: KindToken,
    token: InvocationToken,
    mode: InvocationMode,
    access: for<'a> fn(&'a mut TxImpl) -> &'a mut Tx,
    hooks: HookRunner<H>,
    tools: BTreeMap<String, Rc<dyn AnyToolDeclaration>>,
    kinds: BTreeMap<String, KindToken>,
}
impl<H: 'static, Tx: TaskTx + ?Sized> HostRuntime<H, Tx> {
    fn new(
        host: Rc<Harness>,
        task: Task,
        kind: KindToken,
        token: InvocationToken,
        mode: InvocationMode,
        access: for<'a> fn(&'a mut TxImpl) -> &'a mut Tx,
    ) -> Result<Self> {
        let weak = Rc::downgrade(&host);
        let ancestry = weak.clone();
        let reporter = weak.clone();
        let runners =
            HookRunners::new(
                move || {
                    let Some(host) = weak.upgrade() else {
                        return Vec::new();
                    };
                    host.hooks
                        .borrow()
                        .iter()
                        .filter(|r| {
                            r.namespace.is_registered()
                                && host
                                    .session
                                    .namespaces
                                    .borrow()
                                    .get(r.namespace.id())
                                    .is_some_and(|n| n.token.same_identity(&r.namespace))
                        })
                        .filter_map(|r| {
                            r.handlers.clone().downcast::<H>().ok().map(|handlers| {
                                HookRegistration {
                                    namespace: r.namespace.clone(),
                                    kind: r.kind.clone(),
                                    handlers,
                                    conversation_id: r.conversation_id,
                                    subtree: r.subtree,
                                }
                            })
                        })
                        .collect()
                },
                move |id| {
                    ancestry
                        .upgrade()
                        .map(|h| h.session.ancestors(id))
                        .unwrap_or_default()
                },
                move |error| {
                    if let Some(h) = reporter.upgrade() {
                        view::report(&h.options.on_report, &error);
                    }
                },
            );
        let hooks = runners.runner(kind.clone(), task.id, task.conversation_id);
        let tools = host.tools.map.borrow().clone();
        let kinds = host
            .session
            .kinds
            .borrow()
            .iter()
            .map(|(n, k)| (n.clone(), k.token.clone()))
            .collect();
        Ok(Self {
            host,
            task,
            kind,
            token,
            mode,
            access,
            hooks,
            tools,
            kinds,
        })
    }
    fn invoker(&self) -> Invoker {
        Invoker::Task {
            token: self.token.clone(),
            id: self.task.id,
            conversation_id: self.task.conversation_id,
            kind: self.kind.clone(),
            core: is_core_kind(self.kind.name()),
            mode: self.mode,
        }
    }
    fn docs(&self) -> Vec<DocRef> {
        self.host
            .session
            .live_tasks
            .borrow()
            .get(&self.task.id)
            .map(|t| {
                t.owns
                    .iter()
                    .flat_map(|id| {
                        [
                            DocRef::Rewindable {
                                conversation_id: *id,
                            },
                            DocRef::Sticky {
                                conversation_id: *id,
                            },
                        ]
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    async fn snapshot(&self, id: Id, sticky: bool, ctx: &Context) -> Result<DocumentSnapshot> {
        let reference = if sticky {
            DocRef::Sticky {
                conversation_id: id,
            }
        } else {
            DocRef::Rewindable {
                conversation_id: id,
            }
        };
        self.host
            .session
            .commit(
                self.invoker(),
                |tx, _| Box::pin(async move { tx.snapshot(reference) }),
                ctx,
                CommitOptions {
                    docs: vec![if sticky {
                        DocRef::Sticky {
                            conversation_id: id,
                        }
                    } else {
                        DocRef::Rewindable {
                            conversation_id: id,
                        }
                    }],
                    closing: false,
                },
            )
            .await
            .map(|r| r.value)
    }
}
impl<H: 'static, Tx: TaskTx + ?Sized> Runtime<H, Tx> for HostRuntime<H, Tx> {
    fn task_id(&self) -> Id {
        self.task.id
    }
    fn conversation_id(&self) -> Id {
        self.task.conversation_id
    }
    fn kind(&self) -> &KindToken {
        &self.kind
    }
    fn commit<'a>(
        &'a self,
        callback: CommitCallback<'a, Tx>,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<JsonValue>> {
        Box::pin(async move {
            let id = self.task.id;
            let access = self.access;
            self.host
                .session
                .commit_scoped(
                    self.invoker(),
                    async move |tx: &mut TxImpl, c: Context| {
                        let task = tx
                            .task(id)
                            .await?
                            .ok_or_else(|| Error::Forbidden("task is not live".into()))?;
                        callback(access(tx), &task, &c).await
                    },
                    ctx,
                    CommitOptions {
                        docs: self.docs(),
                        closing: false,
                    },
                )
                .await
                .map(|r| r.value)
        })
    }
    fn hooks(&self) -> &HookRunner<H> {
        &self.hooks
    }
    fn models(&self) -> &dyn Models {
        self.host.options.models.as_ref()
    }
    fn tools(&self) -> &BTreeMap<String, Rc<dyn AnyToolDeclaration>> {
        &self.tools
    }
    fn kinds(&self) -> &BTreeMap<String, KindToken> {
        &self.kinds
    }
    fn registries(&self) -> RuntimeRegistries<'_> {
        RuntimeRegistries {
            sections: &self.host.sections,
            tools: &self.host.tools,
        }
    }
    fn process_host(&self) -> Option<&dyn ProcessHost> {
        self.host.options.process_host.as_deref()
    }
    fn plugins(&self) -> &BTreeMap<String, Rc<dyn PluginHandler>> {
        &self.host.options.plugins
    }
    fn now(&self) -> u64 {
        (self.host.options.now)()
    }
    fn sleep<'a>(&'a self, until_ms: u64, ctx: &'a Context) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            super::session::check_abort(ctx)?;
            let delay =
                tokio::time::sleep(Duration::from_millis(until_ms.saturating_sub(self.now())));
            if let Some(signal) = ctx.abort_signal() {
                tokio::select! { biased; () = signal.cancelled() => Err(Error::Forbidden("sleep aborted".into())), () = delay => Ok(()) }
            } else {
                delay.await;
                Ok(())
            }
        })
    }
    fn wait_for_input<'a>(&'a self, id: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Input>> {
        Box::pin(self.host.scheduler.wait_for_input(id, ctx))
    }
    fn wait_for_task<'a>(&'a self, id: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<Task>> {
        Box::pin(self.host.scheduler.wait_for_task(id, ctx))
    }
    fn abort_task<'a>(
        &'a self,
        id: Id,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<AbortTaskResult>> {
        Box::pin(async move {
            self.host
                .session
                .on_line(ctx, |c| {
                    Box::pin(async move {
                        self.host
                            .assert_invocation(self.task.id, &self.token, self.mode)?;
                        let task = self
                            .host
                            .session
                            .storage
                            .task(id, &c)
                            .await?
                            .ok_or_else(|| Error::InvalidShape("task missing".into()))?;
                        self.host
                            .assert_owned(self.task.id, task.conversation_id, true)
                    })
                })
                .await?;
            self.host.scheduler.abort_task(id, ctx).await
        })
    }
    fn abort_conversation<'a>(
        &'a self,
        id: Id,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.host
                .session
                .on_line(ctx, |_| {
                    Box::pin(async move {
                        self.host
                            .assert_invocation(self.task.id, &self.token, self.mode)?;
                        self.host.assert_owned(self.task.id, id, false)
                    })
                })
                .await?;
            self.host.handle(id).abort(ctx).await
        })
    }
    fn create_owned_conversation<'a>(
        &'a self,
        spec: OwnedConversationSpec,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Id>> {
        Box::pin(async move {
            self.host
                .session
                .commit_scoped(
                    Invoker::Kernel {
                        conversation_id: None,
                    },
                    async move |tx: &mut TxImpl, _: Context| {
                        self.host
                            .assert_invocation(self.task.id, &self.token, self.mode)?;
                        tx.create_owned_conversation(self.task.id, self.task.conversation_id, spec)
                            .await
                    },
                    ctx,
                    CommitOptions::default(),
                )
                .await
                .map(|r| r.value)
        })
    }
    fn send_owned<'a>(
        &'a self,
        id: Id,
        input: SendInput,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Id>> {
        Box::pin(async move {
            self.host
                .session
                .commit_scoped(
                    Invoker::Kernel {
                        conversation_id: None,
                    },
                    async move |tx: &mut TxImpl, _: Context| {
                        self.host
                            .assert_invocation(self.task.id, &self.token, self.mode)?;
                        self.host.assert_owned(self.task.id, id, false)?;
                        tx.send(id, input).await
                    },
                    ctx,
                    CommitOptions {
                        docs: vec![
                            DocRef::Rewindable {
                                conversation_id: id,
                            },
                            DocRef::Sticky {
                                conversation_id: id,
                            },
                        ],
                        closing: false,
                    },
                )
                .await
                .map(|r| r.value)
        })
    }
    fn context<'a>(
        &'a self,
        id: Id,
        at: Option<Id>,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<ContextView>> {
        Box::pin(async move {
            self.host
                .session
                .commit(
                    self.invoker(),
                    |tx, _| Box::pin(async move { tx.context(id, at).await }),
                    ctx,
                    CommitOptions::default(),
                )
                .await
                .map(|r| r.value)
        })
    }
    fn newest_entry<'a>(
        &'a self,
        id: Id,
        opts: NewestEntryOptions,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Option<Entry>>> {
        Box::pin(async move {
            self.host
                .session
                .commit(
                    self.invoker(),
                    |tx, _| {
                        Box::pin(async move {
                            tx.newest_entry(EntryScan {
                                conversation_id: id,
                                kind: opts.kind,
                                with_head: opts.with_head,
                                before: None,
                                limit: 1,
                            })
                            .await
                        })
                    },
                    ctx,
                    CommitOptions::default(),
                )
                .await
                .map(|r| r.value)
        })
    }
    fn rewindable<'a>(
        &'a self,
        id: Id,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<RewindableState>> {
        Box::pin(async move {
            match self.snapshot(id, false, ctx).await? {
                DocumentSnapshot::Rewindable(s) => Ok(s),
                _ => Err(Error::InvalidShape("wrong document".into())),
            }
        })
    }
    fn sticky<'a>(&'a self, id: Id, ctx: &'a Context) -> LocalBoxFuture<'a, Result<StickyState>> {
        Box::pin(async move {
            match self.snapshot(id, true, ctx).await? {
                DocumentSnapshot::Sticky(s) => Ok(s),
                _ => Err(Error::InvalidShape("wrong document".into())),
            }
        })
    }
    fn rewindable_as_of<'a>(
        &'a self,
        id: Id,
        at: Id,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, Result<Option<RewindableState>>> {
        Box::pin(async move {
            self.host
                .session
                .commit(
                    self.invoker(),
                    |tx, _| Box::pin(async move { tx.rewindable_as_of(id, at).await }),
                    ctx,
                    CommitOptions::default(),
                )
                .await
                .map(|r| r.value)
        })
    }
}

#[cfg(test)]
#[path = "harness_tests.rs"]
pub(crate) mod tests;
