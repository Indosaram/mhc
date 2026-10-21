use super::{errors::{callback_panic, collected, contract, share, SharedServiceError, RemoteServiceError, RemoteServiceErrorCode as Code}, handle::AccessGuard, instances::{InstanceDirectory, InstanceDirectoryEntry}, state::{lock, report_error, service_delivery_context, ReplicatedStateReplica}};
use crate::{context::BACKGROUND_CONTEXT, delta::Op, types::*};
use std::{collections::BTreeMap, sync::{Arc, Mutex, Weak, atomic::{AtomicBool, AtomicU64, Ordering}}};
use tokio::sync::watch;
use futures::FutureExt;

fn failure(code: Code, message: &str) -> ServiceError { Box::new(RemoteServiceError::new(code, message)) }
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind { Method, State }
struct MemberDescription { kind: Option<Kind>, expected: Option<Kind> }
pub struct MemberSlot {
    id: JsString, address: Option<ServiceInstanceAddress>, transport: Arc<dyn RemoteServiceTransport>, access: AccessGuard,
    active: Arc<dyn Fn() -> bool + Send + Sync>, name: JsString, description: Mutex<MemberDescription>, state: Arc<ReplicatedStateReplica>,
}
impl MemberSlot {
    fn expect(&self, kind: Kind) -> Result<(), ServiceError> {
        let mut d = lock(&self.description);
        if d.expected.is_some_and(|k| k != kind) || d.kind.is_some_and(|k| k != kind) { return Err(failure(Code::ServiceMemberMismatch, "Remote service member kind mismatch")); }
        d.expected = Some(kind); Ok(())
    }
    fn describe(&self, kind: Kind) -> Result<(), ServiceError> {
        let mut d = lock(&self.description);
        if d.kind.is_some_and(|k| k != kind) || d.expected.is_some_and(|k| k != kind) { return Err(failure(Code::ServiceMemberMismatch, "Remote service member changed kind")); }
        d.kind = Some(kind); Ok(())
    }
    fn access(&self) -> Result<(), ServiceError> { (self.access)() }
    pub fn value(&self) -> Result<Option<Arc<JsonValue>>, ServiceError> { self.access()?; self.expect(Kind::State)?; Ok(self.state.value()) }
    pub fn subscribe(&self, listener: StateListener<JsonValue>) -> Result<Disposal, ServiceError> { self.access()?; self.expect(Kind::State)?; Ok(self.state.subscribe(listener)) }
    pub async fn invoke(&self, args: Vec<JsonValue>, context: ContextRef) -> Result<Option<JsonValue>, ServiceError> {
        self.access()?; self.expect(Kind::Method)?;
        if !(self.active)() { return Err(failure(Code::ServiceStaleInstance, "Remote service binding is closed")); }
        self.transport.invoke(ServiceCall { service_id: self.id.clone(), instance: self.address.clone(), member: self.name.clone(), args }, context).await
    }
}
pub struct ServiceFacade {
    id: JsString, address: Option<ServiceInstanceAddress>, transport: Arc<dyn RemoteServiceTransport>, report: ErrorHandler,
    slots: Mutex<BTreeMap<JsString, Arc<MemberSlot>>>, descriptions: Mutex<BTreeMap<JsString, Kind>>, active: Arc<dyn Fn() -> bool + Send + Sync>, access: AccessGuard,
}
impl ServiceFacade {
    fn new(id: JsString, address: Option<ServiceInstanceAddress>, owner: &BindingInner, active: Arc<dyn Fn() -> bool + Send + Sync>) -> Arc<Self> {
        let disposed = owner.disposed.clone(); let access = owner.access.clone();
        Arc::new(Self { id, address, transport: owner.transport.clone(), report: owner.report.clone(), slots: Mutex::new(BTreeMap::new()), descriptions: Mutex::new(BTreeMap::new()), active,
            access: Arc::new(move || { if disposed.load(Ordering::Acquire) { return Err(contract("Remote service binding is disposed")); } access() }) })
    }
    pub fn member(self: &Arc<Self>, name: &JsString) -> Result<Arc<MemberSlot>, ServiceError> {
        let mut slots = lock(&self.slots);
        if let Some(slot) = slots.get(name) { return Ok(slot.clone()); }
        let kind = lock(&self.descriptions).get(name).copied();
        let slot = Arc::new(MemberSlot { id: self.id.clone(), address: self.address.clone(), transport: self.transport.clone(), access: self.access.clone(), active: self.active.clone(), name: name.clone(), description: Mutex::new(MemberDescription { kind, expected: None }), state: ReplicatedStateReplica::new(self.report.clone()) });
        slots.insert(name.clone(), slot.clone()); Ok(slot)
    }
    fn install(self: &Arc<Self>, snapshot: ServiceInstanceSnapshot, context: ContextRef) -> Result<(), ServiceError> {
        if !same_address(&self.address, &snapshot.instance) { return Err(contract("Remote service snapshot has the wrong address")); }
        let mut descriptions = BTreeMap::new();
        for member in &snapshot.members {
            let (name, kind) = match member { ServiceMemberSnapshot::Method { name } => (name, Kind::Method), ServiceMemberSnapshot::State { name, .. } => (name, Kind::State) };
            if name.is_empty() || descriptions.insert(name.clone(), kind).is_some() { return Err(contract("Remote service has invalid member descriptions")); }
        }
        for name in lock(&self.slots).keys() { if !descriptions.contains_key(name) { return Err(failure(Code::ServiceMemberNotFound, "Unknown remote service member")); } }
        *lock(&self.descriptions) = descriptions;
        for member in snapshot.members {
            match member {
                ServiceMemberSnapshot::Method { name } => { let slot = lock(&self.slots).get(&name).cloned(); if let Some(slot) = slot { slot.describe(Kind::Method)?; } }
                ServiceMemberSnapshot::State { name, sequence, ops } => { let slot = self.member(&name)?; slot.describe(Kind::State)?; slot.state.hydrate(sequence, &ops, context.clone())?; }
            }
        } Ok(())
    }
    fn update(self: &Arc<Self>, member: &JsString, sequence: ServiceSequence, ops: &[Op], context: ContextRef) -> Result<(), ServiceError> {
        if lock(&self.descriptions).get(member) != Some(&Kind::State) { return Err(contract("Remote service update targets non-state member")); }
        let slot = self.member(member)?; slot.describe(Kind::State)?; slot.state.update(sequence, ops, context)
    }
    fn clear(&self) { let slots = lock(&self.slots).values().cloned().collect::<Vec<_>>(); for slot in slots { slot.state.clear(); } }
}
struct KeyedInstance { address: ServiceInstanceAddress, facade: Arc<ServiceFacade>, active: Arc<AtomicBool> }
impl InstanceDirectoryEntry for KeyedInstance {
    fn key(&self) -> &JsString { &self.address.key }
    fn generation(&self) -> ServiceSequence { self.address.generation }
    fn service(&self) -> ServiceImplementation { self.facade.clone() }
    fn deactivate(&self) { self.active.store(false, Ordering::Release); self.facade.clear(); }
}
struct Start { completion: watch::Receiver<Option<Result<(), SharedServiceError>>> }
impl Start {
    async fn ready(&self) -> Result<(), ServiceError> {
        let mut receiver = self.completion.clone();
        let result = receiver.wait_for(Option::is_some).await.map_err(|e| contract(e.to_string()))?.clone();
        match result { Some(Ok(())) => Ok(()), Some(Err(error)) => Err(Box::new(error)), None => Err(contract("Service start did not settle")) }
    }
}
enum BindingKind { Singleton(Arc<ServiceFacade>), Keyed(Arc<InstanceDirectory<KeyedInstance>>) }
struct Binding { id: JsString, kind: BindingKind, revision: AtomicU64, subscription: tokio::sync::Mutex<Option<Box<dyn ServiceSubscription>>>, starting: Mutex<Option<Arc<Start>>>, operations: Mutex<Vec<Arc<Start>>>, active: AtomicBool }
impl Binding {
    fn remember(&self, start: Arc<Start>) {
        let mut operations = lock(&self.operations);
        operations.retain(|operation| !matches!(operation.completion.borrow().as_ref(), Some(Ok(()))));
        operations.push(start);
    }
}
async fn close_subscription(subscription: &mut dyn ServiceSubscription, context: ContextRef) -> Result<(), ServiceError> {
    match std::panic::AssertUnwindSafe(async { subscription.close(Some(context)).await }).catch_unwind().await {
        Ok(result) => result,
        Err(payload) => Err(callback_panic("Service subscription close", payload)),
    }
}
fn clear_binding(binding: &Binding, dispose: bool) -> Result<(), ServiceError> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &binding.kind {
        BindingKind::Singleton(facade) => facade.clear(),
        BindingKind::Keyed(directory) => if dispose { directory.dispose(); } else { directory.reset(); },
    })) {
        Ok(()) => Ok(()),
        Err(payload) => Err(callback_panic("Service binding release", payload)),
    }
}
struct BindingInner {
    transport: Arc<dyn RemoteServiceTransport>, allowlist: Vec<JsString>, report: ErrorHandler, access: AccessGuard,
    bindings: Mutex<BTreeMap<JsString, Arc<Binding>>>, disposed: Arc<AtomicBool>, bound: Arc<AtomicBool>, readiness_revision: AtomicU64,
    disposal: Mutex<Option<Arc<Start>>>,
}
pub struct RemoteServiceBindingImpl { inner: Arc<BindingInner> }
impl RemoteServiceBindingImpl {
    pub fn new(options: RemoteServiceBindingOptions) -> Result<Arc<Self>, ServiceError> {
        let mut ids = std::collections::HashSet::new();
        if options.services.iter().any(|id| !ids.insert(id)) { return Err(contract("Remote service binding has duplicate service IDs")); }
        Ok(Arc::new(Self { inner: Arc::new(BindingInner { transport: options.transport, allowlist: options.services, report: options.on_error.unwrap_or_else(|| Arc::new(|_| {})), access: options.assert_access.unwrap_or_else(|| Arc::new(|| Ok(()))), bindings: Mutex::new(BTreeMap::new()), disposed: Arc::new(AtomicBool::new(false)), bound: Arc::new(AtomicBool::new(options.bound)), readiness_revision: AtomicU64::new(0), disposal: Mutex::new(None) }) }))
    }
    pub fn use_facade(&self, id: &JsString) -> Result<Arc<ServiceFacade>, ServiceError> {
        let binding = self.inner.acquire(id, ServiceMode::Singleton)?;
        match &binding.kind { BindingKind::Singleton(facade) => Ok(facade.clone()), BindingKind::Keyed(_) => Err(failure(Code::ServiceModeMismatch, "Remote service mode mismatch")) }
    }
}
impl BindingInner {
    fn acquire(self: &Arc<Self>, id: &JsString, mode: ServiceMode) -> Result<Arc<Binding>, ServiceError> {
        if self.disposed.load(Ordering::Acquire) { return Err(contract("Remote service binding is disposed")); }
        if !self.allowlist.iter().any(|allowed| allowed == id) { return Err(failure(Code::ServiceNotAllowed, "Remote service is not allowlisted")); }
        let mut bindings = lock(&self.bindings);
        if self.disposed.load(Ordering::Acquire) { return Err(contract("Remote service binding is disposed")); }
        if let Some(binding) = bindings.get(id) {
            let current = match binding.kind { BindingKind::Singleton(_) => ServiceMode::Singleton, BindingKind::Keyed(_) => ServiceMode::Keyed };
            if current != mode { return Err(failure(Code::ServiceModeMismatch, "Remote service mode mismatch")); } return Ok(binding.clone());
        }
        let owner = self.clone(); let name = id.to_owned();
        let binding = Arc::new_cyclic(|weak: &Weak<Binding>| {
            let weak = weak.clone(); let bound = owner.bound.clone(); let disposed = owner.disposed.clone();
            let active = Arc::new(move || bound.load(Ordering::Acquire) && !disposed.load(Ordering::Acquire) && weak.upgrade().is_some_and(|b| b.active.load(Ordering::Acquire)));
            Binding { id: name.clone(), kind: match mode { ServiceMode::Singleton => BindingKind::Singleton(ServiceFacade::new(name.clone(), None, &owner, active)), ServiceMode::Keyed => BindingKind::Keyed(InstanceDirectory::new(false, owner.report.clone())) }, revision: AtomicU64::new(0), subscription: tokio::sync::Mutex::new(None), starting: Mutex::new(None), operations: Mutex::new(Vec::new()), active: AtomicBool::new(true) }
        });
        bindings.insert(id.clone(), binding.clone()); drop(bindings);
        self.readiness_revision.fetch_add(1, Ordering::AcqRel);
        if self.bound.load(Ordering::Acquire) && mode == ServiceMode::Singleton { self.start(&binding); }
        Ok(binding)
    }
    fn start(self: &Arc<Self>, binding: &Arc<Binding>) -> Arc<Start> {
        let (sender, completion) = watch::channel(None); let start = Arc::new(Start { completion });
        *lock(&binding.starting) = Some(start.clone());
        binding.remember(start.clone());
        let owner = self.clone(); let binding = binding.clone(); let revision = binding.revision.load(Ordering::Acquire);
        tokio::spawn(async move {
            let result = match std::panic::AssertUnwindSafe(owner.start_binding(&binding, revision)).catch_unwind().await { Ok(result) => result, Err(payload) => Err(callback_panic("Service startup", payload)) };
            sender.send_replace(Some(result.map_err(share)));
            let error = sender.borrow().as_ref().and_then(|result| result.as_ref().err()).cloned();
            if let Some(error) = error && binding.active.load(Ordering::Acquire) && binding.revision.load(Ordering::Acquire) == revision && owner.bound.load(Ordering::Acquire) { report_error(&owner.report, Box::new(error)); }
        }); start
    }
    async fn start_binding(self: &Arc<Self>, binding: &Arc<Binding>, revision: u64) -> Result<(), ServiceError> {
        let owner = Arc::downgrade(self); let target = Arc::downgrade(binding);
        let mode = match binding.kind { BindingKind::Singleton(_) => ServiceMode::Singleton, BindingKind::Keyed(_) => ServiceMode::Keyed };
        let listener = Arc::new(move |update, context| {
            let (Some(owner), Some(binding)) = (owner.upgrade(), target.upgrade()) else { return; };
            if !binding.active.load(Ordering::Acquire) || binding.revision.load(Ordering::Acquire) != revision { return; }
            if let Err(error) = owner.update(&binding, update, context) { report_error(&owner.report, error); }
        });
        let mut subscription = self.transport.subscribe(binding.id.clone(), mode, listener, BACKGROUND_CONTEXT.clone()).await?;
        if !binding.active.load(Ordering::Acquire) || self.disposed.load(Ordering::Acquire) || !self.bound.load(Ordering::Acquire) || binding.revision.load(Ordering::Acquire) != revision { close_subscription(subscription.as_mut(), BACKGROUND_CONTEXT.clone()).await?; return Ok(()); }
        let installation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let snapshot = subscription.snapshot().clone();
            if snapshot.mode != mode || snapshot.service_id != binding.id { return Err(contract("Remote service returned the wrong snapshot")); }
            match &binding.kind {
                BindingKind::Singleton(facade) => { if snapshot.instances.len() != 1 { return Err(contract("Remote service returned an invalid singleton snapshot")); } facade.install(snapshot.instances.into_iter().next().ok_or_else(|| contract("Missing singleton"))?, service_delivery_context())?; }
                BindingKind::Keyed(directory) => { for instance in snapshot.instances { self.spawn(binding, directory, instance, service_delivery_context())?; } }
            }
            subscription.activate()?;
            if let BindingKind::Keyed(directory) = &binding.kind { directory.ready()?; }
            Ok::<(), ServiceError>(())
        }));
        let installation = match installation { Ok(result) => result, Err(payload) => Err(callback_panic("Service subscription initialization", payload)) };
        if let Err(error) = installation {
            let mut errors = vec![error];
            if let Err(error) = close_subscription(subscription.as_mut(), BACKGROUND_CONTEXT.clone()).await { errors.push(error); }
            return collected(errors, "Failed to initialize service subscription");
        }
        if !binding.active.load(Ordering::Acquire) || self.disposed.load(Ordering::Acquire) || !self.bound.load(Ordering::Acquire) || binding.revision.load(Ordering::Acquire) != revision {
            close_subscription(subscription.as_mut(), BACKGROUND_CONTEXT.clone()).await?;
            return Ok(());
        }
        *binding.subscription.lock().await = Some(subscription); Ok(())
    }
    fn spawn(self: &Arc<Self>, binding: &Arc<Binding>, directory: &Arc<InstanceDirectory<KeyedInstance>>, snapshot: ServiceInstanceSnapshot, context: ContextRef) -> Result<(), ServiceError> {
        let address = snapshot.instance.clone().ok_or_else(|| contract("Keyed service instance snapshot has no address"))?;
        let active = Arc::new(AtomicBool::new(true)); let flag = active.clone(); let target = Arc::downgrade(binding);
        let facade = ServiceFacade::new(binding.id.clone(), Some(address.clone()), self, Arc::new(move || flag.load(Ordering::Acquire) && target.upgrade().is_some_and(|b| b.active.load(Ordering::Acquire))));
        facade.install(snapshot, context)?; directory.replace(Arc::new(KeyedInstance { address, facade, active }))
    }
    fn update(self: &Arc<Self>, binding: &Arc<Binding>, update: ServiceProviderUpdate, context: ContextRef) -> Result<(), ServiceError> {
        match &binding.kind {
            BindingKind::Singleton(facade) => match update {
                ServiceProviderUpdate::Reset { snapshot } => { validate_reset(&snapshot, &binding.id, ServiceMode::Singleton)?; if let Some(instance) = snapshot.instances.into_iter().next() { facade.install(instance, context)?; } else { facade.clear(); } }
                ServiceProviderUpdate::Unavailable => facade.clear(),
                ServiceProviderUpdate::Replaced { snapshot } => facade.install(snapshot, context)?,
                ServiceProviderUpdate::State { instance: None, member, sequence, ops } => facade.update(&member, sequence, &ops, context)?,
                ServiceProviderUpdate::State { instance: Some(_), .. } | ServiceProviderUpdate::Spawned { .. } | ServiceProviderUpdate::Closed { .. } => {},
            },
            BindingKind::Keyed(directory) => match update {
                ServiceProviderUpdate::Reset { snapshot } => {
                    validate_reset(&snapshot, &binding.id, ServiceMode::Keyed)?;
                    for entry in directory.values() { if !snapshot.instances.iter().any(|s| same_address(&s.instance, &Some(entry.address.clone()))) { directory.remove(&entry); } }
                    for snapshot in snapshot.instances {
                        let address = snapshot.instance.as_ref().ok_or_else(|| contract("Missing keyed address"))?;
                        if let Some(entry) = directory.get(&address.key) { entry.facade.install(snapshot, context.clone())?; } else { self.spawn(binding, directory, snapshot, context.clone())?; }
                    }
                }
                ServiceProviderUpdate::Unavailable | ServiceProviderUpdate::Replaced { .. } => return Err(contract("Keyed service received a singleton lifecycle update")),
                ServiceProviderUpdate::Spawned { instance } => self.spawn(binding, directory, instance, context)?,
                ServiceProviderUpdate::Closed { instance } => { if let Some(entry) = directory.get(&instance.key) && entry.address.generation == instance.generation { directory.remove(&entry); } }
                ServiceProviderUpdate::State { instance, member, sequence, ops } => { let address = instance.ok_or_else(|| contract("Keyed state update has no instance address"))?; if let Some(entry) = directory.get(&address.key) && entry.address.generation == address.generation { entry.facade.update(&member, sequence, &ops, context)?; } }
            },
        } Ok(())
    }
}
impl RemoteServices for RemoteServiceBindingImpl {
    fn use_service(&self, id: &JsString) -> Result<ServiceImplementation, ServiceError> { Ok(self.use_facade(id)?) }
    fn observe(&self, id: &JsString, handler: ServiceObserver) -> Result<Disposal, ServiceError> {
        let binding = self.inner.acquire(id, ServiceMode::Keyed)?;
        let directory = match &binding.kind { BindingKind::Keyed(directory) => directory.clone(), BindingKind::Singleton(_) => return Err(failure(Code::ServiceModeMismatch, "Remote service mode mismatch")) };
        let stopped = Arc::new(AtomicBool::new(false));
        let observation_closed = stopped.clone();
        let stop = directory.observe(Arc::new(move |service, context| {
            let guarded = match service.downcast::<ServiceFacade>() {
                Ok(facade) => {
                    let context = context.clone(); let stopped = observation_closed.clone();
                    let original = facade.access.clone();
                    let view = Arc::new(ServiceFacade {
                        id: facade.id.clone(), address: facade.address.clone(), transport: facade.transport.clone(), report: facade.report.clone(),
                        slots: Mutex::new(BTreeMap::new()), descriptions: Mutex::new(lock(&facade.descriptions).clone()), active: facade.active.clone(),
                        access: Arc::new(move || { original()?; if stopped.load(Ordering::Acquire) || context.abort_signal().is_some_and(|signal| signal.aborted()) { return Err(failure(Code::ServiceStaleInstance, "Remote service observation is closed")); } Ok(()) }),
                    });
                    let descriptions = lock(&facade.slots).clone();
                    for (name, slot) in descriptions {
                        lock(&view.slots).insert(name.clone(), Arc::new(MemberSlot { id: view.id.clone(), address: view.address.clone(), transport: view.transport.clone(), access: view.access.clone(), active: view.active.clone(), name, description: Mutex::new(MemberDescription { kind: lock(&slot.description).kind, expected: None }), state: slot.state.clone() }));
                    }
                    view as ServiceImplementation
                }
                Err(_) => { return Box::pin(async { Err(contract("Invalid keyed service facade")) }); }
            };
            handler(guarded, context)
        }))?;
        if self.inner.bound.load(Ordering::Acquire) && lock(&binding.starting).is_none() { self.inner.start(&binding); }
        let owner = self.inner.clone(); let id = id.to_owned();
        Ok(Box::new(move || { stopped.store(true, Ordering::Release); stop();
            let cleanup = tokio::spawn(async move {
            if directory.observer_count() == 0 {
                let removed = { let mut bindings = lock(&owner.bindings); if bindings.get(&id).is_some_and(|b| Arc::ptr_eq(b, &binding)) { bindings.remove(&id); true } else { false } };
                if removed {
                    binding.active.store(false, Ordering::Release); binding.revision.fetch_add(1, Ordering::AcqRel);
                    owner.readiness_revision.fetch_add(1, Ordering::AcqRel);
                    let mut errors = clear_binding(&binding, true).err().into_iter().collect::<Vec<_>>();
                    let starts = std::mem::take(&mut *lock(&binding.operations));
                    let closing = async { let subscription = binding.subscription.lock().await.take(); match subscription { Some(mut subscription) => close_subscription(subscription.as_mut(), BACKGROUND_CONTEXT.clone()).await, None => Ok(()) } };
                    let (closed, starts) = tokio::join!(closing, futures::future::join_all(starts.iter().map(|start| start.ready())));
                    errors.extend(starts.into_iter().chain(std::iter::once(closed)).filter_map(Result::err));
                    collected(errors, "Failed to release service observation")?;
                }
            } Ok(())
        });
            Box::pin(async move { cleanup.await? })
        }))
    }
    fn ready(&self, context: ContextRef) -> BoxFuture<'_, Result<(), ServiceError>> {
        Box::pin(async move {
            loop {
                if self.inner.disposed.load(Ordering::Acquire) { return Err(contract("Remote service binding is disposed")); }
                let revision = self.inner.readiness_revision.load(Ordering::Acquire);
                let waiting = async {
                    let starts = lock(&self.inner.bindings).values().filter_map(|b| lock(&b.starting).clone()).collect::<Vec<_>>();
                    let results = futures::future::join_all(starts.iter().map(|start| start.ready())).await;
                    collected(results.into_iter().filter_map(Result::err).collect(), "Failed to ready services")?;
                    Ok::<(), ServiceError>(())
                };
                match context.abort_signal() { None => waiting.await?, Some(signal) => { if signal.aborted() { return Err(crate::context::cancellation_error(signal.as_ref())); } tokio::select! { biased; () = signal.cancelled() => return Err(crate::context::cancellation_error(signal.as_ref())), result = waiting => result?, } } }
                if self.inner.disposed.load(Ordering::Acquire) { return Err(contract("Remote service binding is disposed")); }
                if revision == self.inner.readiness_revision.load(Ordering::Acquire) { return Ok(()); }
            }
        })
    }
    fn dispose(&self, context: ContextRef) -> BoxFuture<'_, Result<(), ServiceError>> {
        let completion = {
            let mut disposal = lock(&self.inner.disposal);
            if let Some(completion) = disposal.as_ref() { completion.clone() }
            else {
                let (sender, receiver) = watch::channel(None);
                let completion = Arc::new(Start { completion: receiver });
                *disposal = Some(completion.clone());
                self.inner.disposed.store(true, Ordering::Release);
                self.inner.readiness_revision.fetch_add(1, Ordering::AcqRel);
                let bindings = std::mem::take(&mut *lock(&self.inner.bindings));
                for binding in bindings.values() { binding.active.store(false, Ordering::Release); binding.revision.fetch_add(1, Ordering::AcqRel); }
                let context = crate::context::without_abort_signal(&context);
                tokio::spawn(async move {
                    let mut errors = Vec::new();
                    for binding in bindings.values() { if let Err(error) = clear_binding(binding, true) { errors.push(error); } }
                    let results = futures::future::join_all(bindings.into_values().map(|binding| { let context = context.clone(); async move {
                        let starts = std::mem::take(&mut *lock(&binding.operations));
                        let closing = async {
                            let subscription = binding.subscription.lock().await.take();
                            match subscription { Some(mut subscription) => close_subscription(subscription.as_mut(), context).await, None => Ok(()) }
                        };
                        let waiting = futures::future::join_all(starts.iter().map(|start| start.ready()));
                        let (closed, mut results) = tokio::join!(closing, waiting);
                        results.push(closed);
                        collected(results.into_iter().filter_map(Result::err).collect(), "Failed to release service binding")
                    } })).await;
                    errors.extend(results.into_iter().filter_map(Result::err));
                    sender.send_replace(Some(collected(errors, "Failed to dispose services").map_err(share)));
                });
                completion
            }
        };
        Box::pin(async move { completion.ready().await })
    }
}
impl RemoteServiceBinding for RemoteServiceBindingImpl {
    fn rebind(&self, bound: bool, context: ContextRef) -> BoxFuture<'_, Result<(), ServiceError>> {
        Box::pin(async move {
            if self.inner.disposed.load(Ordering::Acquire) { return Err(contract("Remote service binding is disposed")); }
            self.inner.bound.store(bound, Ordering::Release); self.inner.readiness_revision.fetch_add(1, Ordering::AcqRel);
            let bindings = lock(&self.inner.bindings).values().cloned().collect::<Vec<_>>(); let mut errors = Vec::new();
            let mut starts = Vec::new();
            for binding in bindings {
                let revision = binding.revision.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
                let release = clear_binding(&binding, false);
                let (sender, completion) = watch::channel(None);
                let start = Arc::new(Start { completion });
                *lock(&binding.starting) = Some(start.clone()); binding.remember(start.clone()); starts.push(start);
                let owner = self.inner.clone(); let context = context.clone();
                tokio::spawn(async move {
                    let mut errors = release.err().into_iter().collect::<Vec<_>>();
                    let subscription = binding.subscription.lock().await.take();
                    if let Some(mut subscription) = subscription && let Err(error) = close_subscription(subscription.as_mut(), crate::context::without_abort_signal(&context)).await { errors.push(error); }
                    let should_start = match &binding.kind { BindingKind::Singleton(_) => true, BindingKind::Keyed(directory) => directory.observer_count() > 0 };
                    if bound && should_start && binding.active.load(Ordering::Acquire) && !owner.disposed.load(Ordering::Acquire) && binding.revision.load(Ordering::Acquire) == revision {
                        match std::panic::AssertUnwindSafe(owner.start_binding(&binding, revision)).catch_unwind().await { Ok(Ok(())) => {}, Ok(Err(error)) => errors.push(error), Err(payload) => errors.push(callback_panic("Service startup", payload)) }
                    }
                    sender.send_replace(Some(collected(errors, "Failed to rebind service").map_err(share)));
                });
            }
            for result in futures::future::join_all(starts.iter().map(|start| start.ready())).await { if let Err(error) = result { errors.push(error); } }
            collected(errors, "Failed to rebind services")
        })
    }
}
fn same_address(a: &Option<ServiceInstanceAddress>, b: &Option<ServiceInstanceAddress>) -> bool { match (a,b) { (None,None) => true, (Some(a),Some(b)) => a.key == b.key && a.generation == b.generation, (Some(_),None) | (None,Some(_)) => false } }
fn validate_reset(snapshot: &ServiceSubscriptionSnapshot, id: &JsString, mode: ServiceMode) -> Result<(), ServiceError> {
    if &snapshot.service_id != id || snapshot.mode != mode || (mode == ServiceMode::Singleton && snapshot.instances.len() > 1) { return Err(contract("Remote service reset has the wrong service or mode")); }
    let mut keys = std::collections::HashSet::new();
    for instance in &snapshot.instances {
        if (mode == ServiceMode::Singleton) == instance.instance.is_some() { return Err(contract("Remote service reset has an invalid instance address")); }
        if let Some(address) = &instance.instance && !keys.insert(&address.key) { return Err(contract("Remote service reset repeats an instance key")); }
        for member in &instance.members { if let ServiceMemberSnapshot::State { ops, .. } = member && !matches!(ops.as_slice(), [Op::Replace(_)]) { return Err(contract("Remote service reset must contain full root replacements")); } }
    } Ok(())
}
