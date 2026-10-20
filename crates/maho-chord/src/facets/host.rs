//! Port of `src/facets/host.ts`, in upstream lifecycle/declaration order.
//! SIZE_OK: the generation state machine and its private runtime records mirror one TS module.
use crate::{
    context::BACKGROUND_CONTEXT,
    services::{
        consumer::{RemoteServiceBindingImpl, ServiceFacade},
        errors::{collected, contract, CollectedErrors},
        handle::{AccessGuard, ServiceObject, ServiceSlot, ServiceValue, ValueView},
        instances::{InstanceDirectory, InstanceDirectoryEntry},
        loopback::create_loopback_service_transport,
        provider::{ProviderImplementation, RemoteServiceProvider, ServiceProviderDefinition},
        state::{lock, MutableReplicatedStateImpl},
        state_internals::Unsubscribe,
    },
    types::*,
};
use std::{collections::{BTreeMap, BTreeSet, VecDeque}, sync::{Arc, Mutex, Weak, atomic::{AtomicBool, Ordering}}};
use futures::FutureExt;

// A caller owns cancellation, not the cleanup future. Once started, a transition
// keeps running on the entered runtime until rollback or retirement has settled.
struct TransitionWaiter(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for TransitionWaiter {
    fn drop(&mut self) { if let Some(cancel) = self.0.take() { match cancel.send(()) { Ok(()) | Err(()) => {} } } }
}
struct StartupOwnership(Option<Arc<FacetKernel>>);
impl Drop for StartupOwnership {
    fn drop(&mut self) {
        if let Some(kernel) = self.0.take() {
            drop(tokio::spawn(async move { for error in kernel.terminate(&[]).await { (kernel.report)(error); } }));
        }
    }
}
async fn transition(
    report: ErrorHandler,
    startup: Option<Arc<FacetKernel>>,
    operation: impl FnOnce(tokio::sync::oneshot::Receiver<()>) -> BoxFuture<'static, Result<(), ServiceError>> + Send + 'static,
) -> Result<(), ServiceError> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| contract("Facet transitions require an entered Tokio runtime"))?;
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let mut waiter = TransitionWaiter(Some(cancel));
    let (complete, completed) = tokio::sync::oneshot::channel();
    drop(runtime.spawn(async move {
        let result = operation(cancelled).await;
        let ownership = StartupOwnership(if result.is_ok() { startup } else { None });
        if let Err((Err(error), _ownership)) = complete.send((result, ownership)) { (report)(error); }
    }));
    let (result, mut ownership) = completed.await.map_err(|_| contract("Facet transition task did not settle"))?;
    drop(ownership.0.take());
    drop(waiter.0.take());
    result
}

#[derive(Clone, PartialEq, Eq)]
struct FacetServiceReference { service: ServiceDescriptor, mode: ServiceMode }
struct ExternalService { service: ServiceDescriptor, mode: ServiceMode, source: usize }
struct FacetRuntime {
    id: String,
    requires: Mutex<Vec<FacetServiceReference>>,
    provides: Mutex<Vec<FacetServiceReference>>,
    lifecycle: Arc<FacetLifecycle>,
    provisions: Mutex<Vec<FacetProvision>>,
    singleton_views: Mutex<BTreeMap<JsString, Arc<FacetServiceHandle>>>,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LifecycleState { SettingUp, Prepared, Active, Disposing, Dead }
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GenerationPhase { Setup, Assembling, Connecting, Activating, Active, Reloading, Disposing, Dead }
type Observation = Box<dyn FnOnce() -> Result<Disposal, ServiceError> + Send>;
struct LifecycleData { state: LifecycleState, service_access: bool, effects: Vec<Disposal>, observations: Vec<Observation>, activate: Vec<Disposal> }
struct FacetLifecycle { id: String, data: Mutex<LifecycleData> }
impl FacetLifecycle {
    fn new(id: &str) -> Arc<Self> { Arc::new(Self { id: id.into(), data: Mutex::new(LifecycleData { state: LifecycleState::SettingUp, service_access: false, effects: Vec::new(), observations: Vec::new(), activate: Vec::new() }) }) }
    fn setting_up(&self, operation: &str) -> Result<(), ServiceError> {
        if lock(&self.data).state != LifecycleState::SettingUp { return Err(contract(format!("Facet {} can {operation} only during setup", self.id))); } Ok(())
    }
    fn running(&self, operation: &str) -> Result<(), ServiceError> {
        if !matches!(lock(&self.data).state, LifecycleState::SettingUp | LifecycleState::Active) { return Err(contract(format!("Facet {} cannot {operation} outside setup or activation", self.id))); } Ok(())
    }
    fn active(&self) -> Result<(), ServiceError> {
        if lock(&self.data).state != LifecycleState::Active { return Err(contract(format!("Facet {} can spawn service instances only while active", self.id))); } Ok(())
    }
    fn access(&self) -> Result<(), ServiceError> {
        let d = lock(&self.data);
        if !d.service_access { return Err(contract(format!("Facet {} service handles cannot be used while {:?}", self.id, d.state))); } Ok(())
    }
    fn revoke(&self) { lock(&self.data).service_access = false; }
    fn own(&self, disposal: Disposal) -> Result<(), ServiceError> { self.running("own resources")?; lock(&self.data).effects.push(disposal); Ok(()) }
    fn observe(&self, start: Observation) -> Result<(), ServiceError> { self.setting_up("observe services")?; lock(&self.data).observations.push(start); Ok(()) }
    fn on_activate(&self, callback: Disposal) -> Result<(), ServiceError> { self.setting_up("register activation callbacks")?; lock(&self.data).activate.push(callback); Ok(()) }
    fn prepared(&self) -> Result<(), ServiceError> { self.setting_up("finish setup")?; lock(&self.data).state = LifecycleState::Prepared; Ok(()) }
    async fn activate(&self) -> Result<(), ServiceError> {
        let (observations, callbacks) = {
            let mut d = lock(&self.data);
            if d.state != LifecycleState::Prepared { return Err(contract("Facet is not prepared")); }
            d.state = LifecycleState::Active; d.service_access = true;
            (std::mem::take(&mut d.observations), std::mem::take(&mut d.activate))
        };
        for start in observations { let effect = start()?; lock(&self.data).effects.push(effect); }
        for callback in callbacks { run_disposal(callback).await?; } Ok(())
    }
    async fn dispose(&self) -> Result<(), ServiceError> {
        let effects = { let mut d = lock(&self.data); if d.state == LifecycleState::Dead { return Ok(()); } d.state = LifecycleState::Disposing; std::mem::take(&mut d.effects) };
        let mut errors = Vec::new();
        for effect in effects.into_iter().rev() { if let Err(error) = run_disposal(effect).await { errors.push(error); } }
        { let mut d = lock(&self.data); d.observations.clear(); d.activate.clear(); d.service_access = false; d.state = LifecycleState::Dead; }
        collected(errors, "Failed to dispose facet")
    }
}

struct LocalInstance { key: JsString, generation: ServiceSequence, implementation: ServiceImplementation }
impl InstanceDirectoryEntry for LocalInstance {
    fn key(&self) -> &JsString { &self.key }
    fn generation(&self) -> ServiceSequence { self.generation }
    fn service(&self) -> ServiceImplementation { self.implementation.clone() }
    fn deactivate(&self) {}
}
struct LocalRegistration { generations: Mutex<BTreeMap<JsString, ServiceSequence>>, directory: Arc<InstanceDirectory<LocalInstance>> }
struct LocalKeyedServiceRegistry { registrations: BTreeMap<JsString, LocalRegistration>, disposed: AtomicBool }
impl LocalKeyedServiceRegistry {
    fn new(ids: Vec<JsString>, on_error: ErrorHandler) -> Result<Arc<Self>, ServiceError> {
        let mut registrations = BTreeMap::new();
        for id in ids { if registrations.insert(id, LocalRegistration { generations: Mutex::new(BTreeMap::new()), directory: InstanceDirectory::new(true, on_error.clone()) }).is_some() { return Err(contract("Local keyed service registry has duplicate IDs")); } }
        Ok(Arc::new(Self { registrations, disposed: AtomicBool::new(false) }))
    }
    fn registration(&self, id: &JsString) -> Result<&LocalRegistration, ServiceError> {
        if self.disposed.load(Ordering::Acquire) { return Err(contract("Local keyed service registry is disposed")); }
        self.registrations.get(id).ok_or_else(|| contract(format!("Local keyed service {id:?} is not registered")))
    }
    fn spawn(&self, id: &JsString, key: &JsString, implementation: ServiceImplementation) -> Result<Unsubscribe, ServiceError> {
        if key.is_empty() { return Err(contract("Local service instance key must not be empty")); }
        local_object(&implementation)?;
        let registration = self.registration(id)?;
        if registration.directory.get(key).is_some() { return Err(contract("Local service already has a live instance with key")); }
        let generation = { let mut generations = lock(&registration.generations); let next = generations.get(key).copied().unwrap_or(ServiceSequence::ZERO).next(); generations.insert(key.clone(), next); next };
        let instance = Arc::new(LocalInstance { key: key.clone(), generation, implementation });
        registration.directory.insert(instance.clone())?;
        let directory = registration.directory.clone(); Ok(Box::new(move || directory.remove(&instance)))
    }
    fn observe(&self, id: &JsString, handler: ServiceObserver) -> Result<Disposal, ServiceError> {
        let stop = self.registration(id)?.directory.observe(handler)?;
        Ok(Box::new(move || { stop(); Box::pin(async { Ok(()) }) }))
    }
    fn dispose(&self) { if self.disposed.swap(true, Ordering::AcqRel) { return; } for registration in self.registrations.values() { registration.directory.dispose(); } }
}

#[derive(Clone)]
enum KeyedSource { Local(Arc<LocalKeyedServiceRegistry>), Remote(Arc<dyn RemoteServices>) }
#[derive(Clone)]
enum SingletonTarget { Local(Arc<ServiceObject>), Remote(Arc<ServiceFacade>) }
struct HostServiceSlots { singletons: Mutex<BTreeMap<JsString, Arc<Mutex<Option<SingletonTarget>>>>>, keyed: Mutex<BTreeMap<JsString, KeyedSource>> }
impl HostServiceSlots {
    fn new() -> Arc<Self> { Arc::new(Self { singletons: Mutex::new(BTreeMap::new()), keyed: Mutex::new(BTreeMap::new()) }) }
    fn singleton(&self, service: &ServiceDescriptor, guard: AccessGuard) -> Arc<FacetServiceHandle> {
        let target = lock(&self.singletons).entry(service.id.clone()).or_insert_with(|| Arc::new(Mutex::new(None))).clone();
        Arc::new(FacetServiceHandle { id: service.id.clone(), guard, target, local_slot: ServiceSlot::new(&service.id, !service.local) })
    }
    fn bind(&self, id: &JsString, target: SingletonTarget) { if let Some(slot) = lock(&self.singletons).get(id) { *lock(slot) = Some(target); } }
    fn observe(&self, service: &ServiceDescriptor, guard: AccessGuard, handler: ServiceObserver) -> Result<Disposal, ServiceError> {
        let source = lock(&self.keyed).get(&service.id).cloned().ok_or_else(|| contract(format!("Service {:?} is disconnected", service.id)))?;
        let stopped = Arc::new(AtomicBool::new(false)); let closed = stopped.clone(); let descriptor = service.clone();
        let handler: ServiceObserver = Arc::new(move |implementation, context| {
            let target = if descriptor.local { local_object(&implementation).map(SingletonTarget::Local) } else { implementation.downcast::<ServiceFacade>().map(SingletonTarget::Remote).map_err(|_| contract("Invalid remote service facade")) };
            let target = match target { Ok(target) => target, Err(error) => return Box::pin(async { Err(error) }) };
            let guard = guard.clone(); let closed = closed.clone(); let observation = context.clone();
            let handle = Arc::new(FacetServiceHandle { id: descriptor.id.clone(), guard: Arc::new(move || { guard()?; if closed.load(Ordering::Acquire) || observation.abort_signal().is_some_and(|s| s.aborted()) { return Err(contract("Keyed service observation is closed")); } Ok(()) }), target: Arc::new(Mutex::new(Some(target))), local_slot: ServiceSlot::new(&descriptor.id, !descriptor.local) });
            handler(handle, context)
        });
        let stop = match source { KeyedSource::Local(registry) => registry.observe(&service.id, handler)?, KeyedSource::Remote(binding) => binding.observe(&service.id, handler)? };
        Ok(Box::new(move || { stopped.store(true, Ordering::Release); stop() }))
    }
    fn dispose(&self) { for slot in lock(&self.singletons).values() { *lock(slot) = None; } lock(&self.singletons).clear(); lock(&self.keyed).clear(); }
}

/// Explicit Rust counterpart of the guarded JavaScript service proxy.
pub struct FacetServiceHandle { id: JsString, guard: AccessGuard, target: Arc<Mutex<Option<SingletonTarget>>>, local_slot: Arc<ServiceSlot> }
impl FacetServiceHandle {
    fn target(&self) -> Result<SingletonTarget, ServiceError> { (self.guard)()?; lock(&self.target).clone().ok_or_else(|| contract(format!("Service {:?} is disconnected", self.id))) }
    pub fn member(self: &Arc<Self>, name: &JsString) -> FacetMember { FacetMember { handle: self.clone(), name: name.clone() } }
    pub fn receiver(&self) -> Result<ServiceImplementation, ServiceError> { match self.target()? { SingletonTarget::Local(object) => Ok(object.receiver.clone()), SingletonTarget::Remote(_) => Err(contract("Service is remote")) } }
}
/// Cached member handles resolve the current singleton target on every access.
pub struct FacetMember { handle: Arc<FacetServiceHandle>, name: JsString }
impl FacetMember {
    fn local(&self, object: Arc<ServiceObject>) -> Result<Arc<ValueView>, ServiceError> { self.handle.local_slot.bind(object); self.handle.local_slot.view(self.handle.guard.clone()).member(&self.name) }
    pub async fn invoke(&self, args: Vec<JsonValue>, context: ContextRef) -> Result<Option<JsonValue>, ServiceError> {
        match self.handle.target()? { SingletonTarget::Local(object) => self.local(object)?.invoke(args, context).await, SingletonTarget::Remote(facade) => facade.member(&self.name)?.invoke(args, context).await }
    }
    pub fn value(&self) -> Result<Option<Arc<JsonValue>>, ServiceError> {
        match self.handle.target()? { SingletonTarget::Remote(facade) => facade.member(&self.name)?.value(), SingletonTarget::Local(object) => match self.local(object)?.get()? { ServiceValue::State(state) => Ok(state.value()), ServiceValue::Json(value) => Ok(Some(Arc::new(value))), ServiceValue::Method(_) | ServiceValue::Object(_) => Err(contract("Service member is not a value")) } }
    }
    pub fn subscribe(&self, listener: StateListener<JsonValue>) -> Result<Disposal, ServiceError> {
        match self.handle.target()? { SingletonTarget::Remote(facade) => facade.member(&self.name)?.subscribe(listener), SingletonTarget::Local(object) => match self.local(object)?.get()? { ServiceValue::State(state) => Ok(state.subscribe(listener)), ServiceValue::Method(_) | ServiceValue::Object(_) | ServiceValue::Json(_) => Err(contract("Service member is not replicated state")) } }
    }
}

type Installer = Arc<dyn Fn(&JsString, ServiceImplementation) -> Result<Unsubscribe, ServiceError> + Send + Sync>;
struct StagedInstance { implementation: ServiceImplementation, release: Option<Unsubscribe> }
struct SpawnerData { instances: BTreeMap<JsString, StagedInstance>, installer: Option<Installer> }
struct StagedServiceSpawner { lifecycle: Arc<FacetLifecycle>, service: ServiceDescriptor, data: Arc<Mutex<SpawnerData>> }
impl StagedServiceSpawner {
    fn connect(&self, installer: Installer) -> Result<(), ServiceError> {
        let instances = { let mut d = lock(&self.data); if d.installer.is_some() { return Err(contract("Facet service provider is already connected")); } d.installer = Some(installer.clone()); d.instances.iter().map(|(key, instance)| (key.clone(), instance.implementation.clone())).collect::<Vec<_>>() };
        for (key, implementation) in instances { let release = installer(&key, implementation)?; let mut d = lock(&self.data); if let Some(instance) = d.instances.get_mut(&key) { instance.release = Some(release); } else { drop(d); release(); } } Ok(())
    }
}
impl ServiceSpawner<ServiceImplementation> for Arc<StagedServiceSpawner> {
    fn spawn(&self, key: &JsString, implementation: ServiceImplementation) -> Result<Disposal, ServiceError> {
        self.lifecycle.active()?; if key.is_empty() { return Err(contract("Facet service instance key must not be empty")); }
        if self.service.local { local_object(&implementation)?; } else { remote_object(&implementation)?; }
        let installer = { let mut d = lock(&self.data); if d.instances.contains_key(key) { return Err(contract("Facet service already has a live instance with key")); }
            d.instances.insert(key.clone(), StagedInstance { implementation: implementation.clone(), release: None }); d.installer.clone() };
        if let Some(install) = installer { let release = install(key, implementation)?; let mut d = lock(&self.data); if let Some(instance) = d.instances.get_mut(key) { instance.release = Some(release); } else { drop(d); release(); } }
        let data = self.data.clone(); let key = key.to_owned(); let identity = Arc::new(AtomicBool::new(false));
        let close = move || { if identity.swap(true, Ordering::AcqRel) { return; } let instance = lock(&data).instances.remove(&key); if let Some(release) = instance.and_then(|i| i.release) { release(); } };
        let close: Arc<dyn Fn() + Send + Sync> = Arc::new(close);
        let owned = close.clone(); self.lifecycle.own(Box::new(move || { owned(); Box::pin(async { Ok(()) }) }))?;
        Ok(Box::new(move || { close(); Box::pin(async { Ok(()) }) }))
    }
}
#[derive(Clone)]
enum FacetProvision { Singleton { service: ServiceDescriptor, implementation: ServiceImplementation }, Keyed { service: ServiceDescriptor, spawner: Arc<StagedServiceSpawner> } }
impl FacetProvision { fn service(&self) -> &ServiceDescriptor { match self { Self::Singleton { service, .. } | Self::Keyed { service, .. } => service } } fn mode(&self) -> ServiceMode { match self { Self::Singleton { .. } => ServiceMode::Singleton, Self::Keyed { .. } => ServiceMode::Keyed } } }

pub struct FacetKernel {
    initial: Vec<Arc<dyn Facet>>, sources: Vec<Arc<dyn RemoteServiceSource>>, report: ErrorHandler,
    facets: Mutex<Vec<Arc<FacetRuntime>>>, slots: Arc<HostServiceSlots>,
    bindings: Mutex<BTreeMap<usize, Arc<dyn RemoteServices>>>, order: Mutex<Vec<String>>,
    provider: Mutex<Option<Arc<RemoteServiceProvider>>>, internal: Mutex<Option<Arc<RemoteServiceBindingImpl>>>, local: Mutex<Option<Arc<LocalKeyedServiceRegistry>>>, phase: Arc<Mutex<GenerationPhase>>,
}
impl FacetKernel {
    pub fn new(options: FacetOptions) -> Result<Arc<Self>, ServiceError> {
        validate_ids(&options.facets)?;
        Ok(Arc::new(Self { initial: options.facets, sources: options.service_sources, report: options.on_error.unwrap_or_else(|| Arc::new(|_| {})), facets: Mutex::new(Vec::new()), slots: HostServiceSlots::new(), bindings: Mutex::new(BTreeMap::new()), order: Mutex::new(Vec::new()), provider: Mutex::new(None), internal: Mutex::new(None), local: Mutex::new(None), phase: Arc::new(Mutex::new(GenerationPhase::Setup)) }))
    }
    pub fn provider(&self) -> Result<Arc<RemoteServiceProvider>, ServiceError> { lock(&self.provider).clone().ok_or_else(|| contract("Facet service provider is not assembled")) }
    fn runtime(id: &str) -> Arc<FacetRuntime> { Arc::new(FacetRuntime { id: id.into(), requires: Mutex::new(Vec::new()), provides: Mutex::new(Vec::new()), lifecycle: FacetLifecycle::new(id), provisions: Mutex::new(Vec::new()), singleton_views: Mutex::new(BTreeMap::new()) }) }
    fn setup(self: &Arc<Self>, facet: &Arc<dyn Facet>, record: &Arc<FacetRuntime>) -> Result<(), ServiceError> {
        let mut environment = Environment { kernel: Arc::downgrade(self), record: record.clone() };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| facet.setup(&mut environment))).map_err(|_| contract("Facet setup panicked"))??;
        record.lifecycle.prepared()
    }
    pub async fn activate(self: &Arc<Self>) -> Result<(), ServiceError> {
        let kernel = self.clone();
        transition(self.report.clone(), Some(self.clone()), move |cancelled| Box::pin(async move { kernel.activate_owned(cancelled).await })).await
    }
    async fn activate_owned(self: &Arc<Self>, mut cancelled: tokio::sync::oneshot::Receiver<()>) -> Result<(), ServiceError> {
        if *lock(&self.phase) != GenerationPhase::Setup { return Err(contract("Facet generation startup has already begun")); }
        let result = tokio::select! {
            biased;
            _ = &mut cancelled => Err(contract("Facet startup was cancelled")),
            result = self.start() => result,
        };
        match result { Ok(()) => Ok(()), Err(error) => { let cleanup = self.terminate(&[]).await; Err(with_cleanup(error, cleanup, "Facet generation startup and cleanup failed")) } }
    }
    async fn start(self: &Arc<Self>) -> Result<(), ServiceError> {
        for facet in &self.initial { let record = Self::runtime(facet.id()); lock(&self.facets).push(record.clone()); self.setup(facet, &record)?; }
        *lock(&self.phase) = GenerationPhase::Assembling;
        let records = lock(&self.facets).clone(); let external = self.resolve_external(&records).await?;
        *lock(&self.order) = validate_facets(&records, &external)?;
        self.assemble()?; self.bind(&external)?;
        *lock(&self.phase) = GenerationPhase::Connecting;
        let mut bindings = lock(&self.bindings).values().cloned().collect::<Vec<_>>(); bindings.push(self.internal()?);
        let ready = futures::future::join_all(bindings.iter().map(|binding| async move {
            let future = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| binding.ready(BACKGROUND_CONTEXT.clone())))
                .map_err(|_| contract("Facet service source readiness panicked"))?;
            std::panic::AssertUnwindSafe(future).catch_unwind().await.map_err(|_| contract("Facet service source readiness future panicked"))?
        })).await;
        for result in ready { result?; }
        *lock(&self.phase) = GenerationPhase::Activating;
        let order = lock(&self.order).clone();
        for id in order { let record = records.iter().find(|r| r.id == id).ok_or_else(|| contract("Missing facet runtime"))?; record.lifecycle.activate().await?; }
        *lock(&self.phase) = GenerationPhase::Active; Ok(())
    }
    pub async fn reload(self: &Arc<Self>, facets: Vec<Arc<dyn Facet>>) -> Result<(), ServiceError> {
        let kernel = self.clone();
        transition(self.report.clone(), None, move |cancelled| Box::pin(async move { kernel.reload_owned(facets, cancelled).await })).await
    }
    async fn reload_owned(self: &Arc<Self>, facets: Vec<Arc<dyn Facet>>, mut cancelled: tokio::sync::oneshot::Receiver<()>) -> Result<(), ServiceError> {
        { let mut phase = lock(&self.phase); if *phase != GenerationPhase::Active { return Err(contract(format!("Facet host cannot reload while {phase:?}"))); }
            validate_ids(&facets)?; let current = lock(&self.facets); for facet in &facets { if !current.iter().any(|r| r.id == facet.id()) { return Err(contract(format!("Facet {} is not active", facet.id()))); } } *phase = GenerationPhase::Reloading; }
        let mut staged = Vec::new();
        let setup = (|| { for facet in &facets { let record = Self::runtime(facet.id()); staged.push(record.clone()); self.setup(facet, &record)?;
            let previous = lock(&self.facets).iter().find(|r| r.id == facet.id()).cloned().ok_or_else(|| contract("Missing previous facet"))?;
            if !same_shape(&previous, &record) { return Err(contract(format!("Reloaded facet {} must preserve its service requirements and provisions", facet.id()))); }
            self.validate_replacements(&record)?;
        } Ok::<(), ServiceError>(()) })();
        if let Err(error) = setup { return Err(self.reject_candidates(error, &staged, "Facet reload setup and cleanup failed").await); }
        let order = lock(&self.order).clone(); let candidates = order.iter().filter_map(|id| staged.iter().find(|r| &r.id == id).cloned()).collect::<Vec<_>>();
        let activation = tokio::select! {
            biased;
            _ = &mut cancelled => Err(contract("Facet reload was cancelled before cutover")),
            result = async { for record in &candidates { record.lifecycle.activate().await?; } for record in &candidates { self.validate_replacements(record)?; } Ok::<(), ServiceError>(()) } => result,
        };
        if let Err(error) = activation { return Err(self.reject_candidates(error, &candidates, "Facet reload activation and cleanup failed").await); }
        let previous = { let mut current = lock(&self.facets); let mut previous = Vec::new(); for candidate in &candidates { let slot = current.iter_mut().find(|r| r.id == candidate.id).ok_or_else(|| contract("Missing replaced facet"))?; previous.push(std::mem::replace(slot, candidate.clone())); } previous };
        let cutover = async {
            for candidate in &candidates { let provisions = lock(&candidate.provisions).clone(); for provision in provisions { if let FacetProvision::Singleton { service, implementation } = provision { if service.local { self.slots.bind(&service.id, SingletonTarget::Local(local_object(&implementation)?)); } else { self.provider()?.replace(&service.id, remote_object(&implementation)?)?; } } } }
            collected(dispose_records(&previous).await, "Failed to retire replaced facets")?;
            for candidate in &candidates { let provisions = lock(&candidate.provisions).clone(); for provision in provisions { if let FacetProvision::Keyed { service, spawner } = provision { self.connect_spawner(&service, &spawner)?; } } } Ok::<(), ServiceError>(())
        }.await;
        if let Err(error) = cutover { let cleanup = self.abort(&previous).await; let mut errors = vec![error]; errors.extend(cleanup); return Err(Box::new(CollectedErrors { message: "Facet reload failed after cutover", errors })); }
        *lock(&self.phase) = GenerationPhase::Active; Ok(())
    }
    async fn reject_candidates(&self, error: ServiceError, records: &[Arc<FacetRuntime>], message: &'static str) -> ServiceError {
        let mut cleanup = dispose_records(records).await;
        if cleanup.is_empty() { *lock(&self.phase) = GenerationPhase::Active; return error; }
        cleanup.extend(self.abort(&[]).await); with_cleanup(error, cleanup, message)
    }
    pub async fn dispose(self: &Arc<Self>) -> Result<(), ServiceError> {
        let kernel = self.clone();
        transition(self.report.clone(), None, move |_| Box::pin(async move { kernel.dispose_owned().await })).await
    }
    async fn dispose_owned(&self) -> Result<(), ServiceError> {
        { let mut phase = lock(&self.phase); if *phase == GenerationPhase::Dead { return Ok(()); } if *phase != GenerationPhase::Active { return Err(contract(format!("Facet host cannot be disposed while {phase:?}"))); } *phase = GenerationPhase::Disposing; }
        collected(self.terminate(&[]).await, "Failed to dispose facet generation")
    }
    fn validate_replacements(&self, record: &FacetRuntime) -> Result<(), ServiceError> {
        for provision in lock(&record.provisions).iter() { if let FacetProvision::Singleton { service, implementation } = provision { if !service.local { self.provider()?.validate_replacement(&service.id, &remote_object(implementation)?)?; } } } Ok(())
    }
    async fn resolve_external(&self, records: &[Arc<FacetRuntime>]) -> Result<Vec<ExternalService>, ServiceError> {
        let catalogues = futures::future::join_all(self.sources.iter().map(|s| async move {
            let future = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.catalogue(BACKGROUND_CONTEXT.clone())))
                .map_err(|_| contract("Facet service source catalogue panicked"))?;
            std::panic::AssertUnwindSafe(future).catch_unwind().await.map_err(|_| contract("Facet service source catalogue future panicked"))?
        })).await;
        let mut offered = BTreeMap::new();
        for (source, entries) in catalogues.into_iter().enumerate() { for entry in entries? { if offered.insert(entry.service_id.clone(), (entry.mode, source)).is_some() { return Err(contract(format!("Facet host service {:?} is offered by more than one source", entry.service_id))); } } }
        let local = records.iter().flat_map(|r| lock(&r.provides).iter().map(|p| p.service.id.clone()).collect::<Vec<_>>()).collect::<BTreeSet<_>>();
        let mut external: Vec<ExternalService> = Vec::new();
        for record in records { for requirement in lock(&record.requires).iter() {
            if local.contains(&requirement.service.id) || external.iter().any(|e| e.service.id == requirement.service.id) { continue; }
            let source = match offered.get(&requirement.service.id).copied() { Some(source) => Some(source), None => { let deferred = self.sources.iter().enumerate().filter(|(_, s)| s.accepts_unavailable_services()).map(|(i, _)| i).collect::<Vec<_>>(); if deferred.len() > 1 { return Err(contract(format!("Facet host service {:?} has more than one deferred source", requirement.service.id))); } deferred.first().map(|i| (requirement.mode, *i)) } };
            if let Some((mode, source)) = source { external.push(ExternalService { service: requirement.service.clone(), mode, source }); }
        } }
        let mut groups: Vec<(usize, Vec<JsString>)> = Vec::new(); for entry in &external { if let Some((_, ids)) = groups.iter_mut().find(|(i, _)| *i == entry.source) { ids.push(entry.service.id.clone()); } else { groups.push((entry.source, vec![entry.service.id.clone()])); } }
        for (index, ids) in groups {
            let binding = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.sources[index].open(&ids, self.target_guard(), self.report.clone())))
                .map_err(|_| contract("Facet service source open panicked"))??;
            lock(&self.bindings).insert(index, binding);
        }
        Ok(external)
    }
    fn provisions(&self) -> Vec<FacetProvision> { lock(&self.facets).iter().flat_map(|r| lock(&r.provisions).clone()).collect() }
    fn assemble(&self) -> Result<(), ServiceError> {
        let provisions = self.provisions(); let remote = provisions.iter().filter(|p| !p.service().local).collect::<Vec<_>>();
        let provider = RemoteServiceProvider::new(remote.iter().map(|p| ServiceProviderDefinition { service_id: p.service().id.clone(), local: false, mode: p.mode() }).collect())?;
        let internal = RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: remote.iter().map(|p| p.service().id.clone()).collect(), transport: create_loopback_service_transport(provider.clone()), bound: true, on_error: Some(self.report.clone()), assert_access: Some(self.target_guard()) })?;
        let local = LocalKeyedServiceRegistry::new(provisions.iter().filter(|p| p.service().local && p.mode() == ServiceMode::Keyed).map(|p| p.service().id.clone()).collect(), self.report.clone())?;
        *lock(&self.provider) = Some(provider.clone()); *lock(&self.internal) = Some(internal); *lock(&self.local) = Some(local);
        for provision in provisions { match provision { FacetProvision::Singleton { service, implementation } => { if !service.local { provider.provide(&service.id, remote_object(&implementation)?)?; } }, FacetProvision::Keyed { service, spawner } => self.connect_spawner(&service, &spawner)? } } Ok(())
    }
    fn connect_spawner(&self, service: &ServiceDescriptor, spawner: &StagedServiceSpawner) -> Result<(), ServiceError> {
        let id = service.id.clone();
        if service.local { let local = self.local()?; spawner.connect(Arc::new(move |key, implementation| local.spawn(&id, key, implementation))) }
        else { let provider = self.provider()?; spawner.connect(Arc::new(move |key, implementation| provider.spawn(&id, key, remote_object(&implementation)?))) }
    }
    fn bind(&self, external: &[ExternalService]) -> Result<(), ServiceError> {
        for provision in self.provisions() { match provision { FacetProvision::Singleton { service, implementation } => {
            let target = if service.local { SingletonTarget::Local(local_object(&implementation)?) } else { SingletonTarget::Remote(self.internal()?.use_facade(&service.id)?) }; self.slots.bind(&service.id, target);
        }, FacetProvision::Keyed { service, .. } => { let source = if service.local { KeyedSource::Local(self.local()?) } else { KeyedSource::Remote(self.internal()?) }; lock(&self.slots.keyed).insert(service.id, source); } } }
        for entry in external { let binding = lock(&self.bindings).get(&entry.source).cloned().ok_or_else(|| contract("Service source is not open"))?;
            match entry.mode { ServiceMode::Singleton => {
                let implementation = binding.use_service(&entry.service.id)?;
                let target = match implementation.downcast::<ServiceFacade>() {
                    Ok(facade) => SingletonTarget::Remote(facade),
                    Err(implementation) => SingletonTarget::Local(local_object(&implementation)?),
                };
                self.slots.bind(&entry.service.id, target);
            }, ServiceMode::Keyed => { lock(&self.slots.keyed).insert(entry.service.id.clone(), KeyedSource::Remote(binding)); } }
        } Ok(())
    }
    fn local(&self) -> Result<Arc<LocalKeyedServiceRegistry>, ServiceError> { lock(&self.local).clone().ok_or_else(|| contract("Facet keyed services are not assembled")) }
    fn internal(&self) -> Result<Arc<RemoteServiceBindingImpl>, ServiceError> { lock(&self.internal).clone().ok_or_else(|| contract("Facet remote services are not assembled")) }
    fn target_guard(&self) -> AccessGuard { let phase = self.phase.clone(); Arc::new(move || { if !matches!(*lock(&phase), GenerationPhase::Activating | GenerationPhase::Active | GenerationPhase::Reloading | GenerationPhase::Disposing) { return Err(contract("Facet service targets cannot be used during this phase")); } Ok(()) }) }
    async fn abort(&self, extra: &[Arc<FacetRuntime>]) -> Vec<ServiceError> { for r in lock(&self.facets).iter().chain(extra.iter()) { r.lifecycle.revoke(); } self.terminate(extra).await }
    async fn terminate(&self, extra: &[Arc<FacetRuntime>]) -> Vec<ServiceError> {
        *lock(&self.phase) = GenerationPhase::Disposing;
        let records = std::mem::take(&mut *lock(&self.facets)); let order = lock(&self.order).clone();
        let ordered = if order.is_empty() { records } else { order.iter().filter_map(|id| records.iter().find(|r| &r.id == id).cloned()).collect() };
        let mut errors = dispose_records(&ordered).await; errors.extend(dispose_records(extra).await);
        let local = lock(&self.local).take(); if let Some(local) = local { local.dispose(); }
        let mut bindings = std::mem::take(&mut *lock(&self.bindings)).into_values().collect::<Vec<_>>(); let internal = lock(&self.internal).take(); if let Some(internal) = internal { bindings.push(internal); }
        errors.extend(futures::future::join_all(bindings.iter().map(|binding| async move {
            let future = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| binding.dispose(BACKGROUND_CONTEXT.clone())))
                .map_err(|_| contract("Facet service source disposal panicked"))?;
            std::panic::AssertUnwindSafe(future).catch_unwind().await.map_err(|_| contract("Facet service source disposal future panicked"))?
        })).await.into_iter().filter_map(Result::err));
        self.slots.dispose(); if let Some(provider) = lock(&self.provider).as_ref() { if let Err(error) = provider.dispose() { errors.push(error); } }
        *lock(&self.phase) = GenerationPhase::Dead; errors
    }
}

#[derive(Clone)]
struct Environment { kernel: Weak<FacetKernel>, record: Arc<FacetRuntime> }
impl Environment { fn kernel(&self) -> Result<Arc<FacetKernel>, ServiceError> { self.kernel.upgrade().ok_or_else(|| contract("Facet host is gone")) } }
impl FacetEnvironment for Environment {
    fn clone_environment(&self) -> Arc<dyn FacetEnvironment> { Arc::new(self.clone()) }
    fn provide(&self, service: &ServiceDescriptor, implementation: ServiceImplementation) -> Result<(), ServiceError> {
        self.record.lifecycle.setting_up("provide services")?;
        if service.local { local_object(&implementation)?; } else { remote_object(&implementation)?; }
        record_reference(&self.record.provides, service, ServiceMode::Singleton);
        lock(&self.record.provisions).push(FacetProvision::Singleton { service: service.clone(), implementation }); Ok(())
    }
    fn provide_many(&self, service: &ServiceDescriptor) -> Result<Box<dyn ServiceSpawner<ServiceImplementation>>, ServiceError> {
        self.record.lifecycle.setting_up("provide service instances")?; record_reference(&self.record.provides, service, ServiceMode::Keyed);
        let spawner = Arc::new(StagedServiceSpawner { lifecycle: self.record.lifecycle.clone(), service: service.clone(), data: Arc::new(Mutex::new(SpawnerData { instances: BTreeMap::new(), installer: None })) });
        lock(&self.record.provisions).push(FacetProvision::Keyed { service: service.clone(), spawner: spawner.clone() }); Ok(Box::new(spawner))
    }
    fn use_service(&self, service: &ServiceDescriptor) -> Result<ServiceImplementation, ServiceError> {
        self.record.lifecycle.setting_up("acquire services")?; record_reference(&self.record.requires, service, ServiceMode::Singleton);
        let mut views = lock(&self.record.singleton_views); if let Some(view) = views.get(&service.id) { return Ok(view.clone()); }
        let lifecycle = self.record.lifecycle.clone(); let view = self.kernel()?.slots.singleton(service, Arc::new(move || lifecycle.access())); views.insert(service.id.clone(), view.clone()); Ok(view)
    }
    fn observe(&self, service: &ServiceDescriptor, handler: ServiceObserver) -> Result<(), ServiceError> {
        self.record.lifecycle.setting_up("observe services")?; record_reference(&self.record.requires, service, ServiceMode::Keyed);
        let slots = self.kernel()?.slots.clone(); let service = service.clone(); let lifecycle = self.record.lifecycle.clone();
        self.record.lifecycle.observe(Box::new(move || slots.observe(&service, Arc::new(move || lifecycle.access()), handler)))
    }
    fn replicated_state(&self, initial: JsonValue) -> Result<Arc<dyn MutableReplicatedState>, ServiceError> { self.record.lifecycle.running("create replicated state")?; Ok(MutableReplicatedStateImpl::new(initial)?) }
    fn own(&self, disposal: Disposal) -> Result<(), ServiceError> { self.record.lifecycle.own(disposal) }
    fn on_activate(&self, callback: Disposal) -> Result<(), ServiceError> { self.record.lifecycle.on_activate(callback) }
    fn on_deactivate(&self, callback: Disposal) -> Result<(), ServiceError> { self.record.lifecycle.own(callback) }
}
async fn dispose_records(records: &[Arc<FacetRuntime>]) -> Vec<ServiceError> { let mut errors = Vec::new(); for record in records.iter().rev() { if let Err(error) = record.lifecycle.dispose().await { errors.push(error); } } errors }
fn validate_ids(facets: &[Arc<dyn Facet>]) -> Result<(), ServiceError> { let mut ids = BTreeSet::new(); for facet in facets { if facet.id().is_empty() { return Err(contract("Facet ID must not be empty")); } if !ids.insert(facet.id()) { return Err(contract("Facet IDs must be unique within a generation")); } } Ok(()) }
fn validate_facets(records: &[Arc<FacetRuntime>], external: &[ExternalService]) -> Result<Vec<String>, ServiceError> {
    let mut providers: BTreeMap<JsString, (Option<String>, ServiceMode)> = external.iter().map(|e| (e.service.id.clone(), (None, e.mode))).collect();
    for record in records { for provision in lock(&record.provides).iter() { if let Some((previous, mode)) = providers.get(&provision.service.id) { if *mode != provision.mode { return Err(contract(format!("Service {:?} is provided as both singleton and keyed", provision.service.id))); } return Err(contract(format!("Service {:?} is provided by both {} and {}", provision.service.id, previous.as_deref().unwrap_or("the host"), record.id))); } providers.insert(provision.service.id.clone(), (Some(record.id.clone()), provision.mode)); } }
    let mut dependencies = records.iter().map(|r| (r.id.clone(), BTreeSet::new())).collect::<BTreeMap<_, _>>(); let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for record in records { for requirement in lock(&record.requires).iter() {
        let (provider, mode) = providers.get(&requirement.service.id).ok_or_else(|| contract(format!("Facet {} requires local/{:?}/{:?}, but no facet provides it", record.id, requirement.service.id, requirement.mode)))?;
        if *mode != requirement.mode { return Err(contract(format!("Facet {} requires {:?} as {:?}, but the provider provides it as {mode:?}", record.id, requirement.service.id, requirement.mode))); }
        if let Some(provider) = provider { if provider != &record.id { let deps = dependencies.get_mut(&record.id).ok_or_else(|| contract("Missing dependency record"))?; if deps.insert(provider.clone()) { dependents.entry(provider.clone()).or_default().push(record.id.clone()); } } }
    } }
    topological_order(records, dependencies, dependents)
}
fn topological_order(records: &[Arc<FacetRuntime>], dependencies: BTreeMap<String, BTreeSet<String>>, dependents: BTreeMap<String, Vec<String>>) -> Result<Vec<String>, ServiceError> {
    let mut remaining = dependencies.into_iter().map(|(id, deps)| (id, deps.len())).collect::<BTreeMap<_, _>>();
    let mut ready = records.iter().filter(|r| remaining.get(&r.id) == Some(&0)).map(|r| r.id.clone()).collect::<VecDeque<_>>(); let mut order = Vec::new();
    while let Some(id) = ready.pop_front() { order.push(id.clone()); for dependent in dependents.get(&id).into_iter().flatten() { let count = remaining.get_mut(dependent).ok_or_else(|| contract("Missing dependent record"))?; *count -= 1; if *count == 0 { ready.push_back(dependent.clone()); } } }
    if order.len() != records.len() { return Err(contract(format!("Facet dependency cycle: {}", records.iter().filter(|r| remaining.get(&r.id).is_some_and(|count| *count > 0)).map(|r| r.id.as_str()).collect::<Vec<_>>().join(", ")))); } Ok(order)
}
fn record_reference(target: &Mutex<Vec<FacetServiceReference>>, service: &ServiceDescriptor, mode: ServiceMode) { let mut target = lock(target); if !target.iter().any(|r| r.service.id == service.id && r.mode == mode) { target.push(FacetServiceReference { service: service.clone(), mode }); } }
fn same_shape(left: &FacetRuntime, right: &FacetRuntime) -> bool { same_references(&lock(&left.requires), &lock(&right.requires)) && same_references(&lock(&left.provides), &lock(&right.provides)) }
fn same_references(left: &[FacetServiceReference], right: &[FacetServiceReference]) -> bool { left.len() == right.len() && left.iter().all(|r| right.iter().any(|other| other.service == r.service && other.mode == r.mode)) }
fn local_object(value: &ServiceImplementation) -> Result<Arc<ServiceObject>, ServiceError> { value.clone().downcast().map_err(|_| contract("Local service implementation must be a ServiceObject")) }
fn remote_object(value: &ServiceImplementation) -> Result<Arc<ProviderImplementation>, ServiceError> { let object = value.clone().downcast::<ProviderImplementation>().map_err(|_| contract("Remote service implementation must be a ProviderImplementation"))?; if object.members.is_empty() { return Err(contract("Remote service has no members")); } Ok(object) }
fn with_cleanup(error: ServiceError, cleanup: Vec<ServiceError>, message: &'static str) -> ServiceError { if cleanup.is_empty() { error } else { let mut errors = vec![error]; errors.extend(cleanup); Box::new(CollectedErrors { message, errors }) } }
async fn run_disposal(disposal: Disposal) -> Result<(), ServiceError> {
    let future = std::panic::catch_unwind(std::panic::AssertUnwindSafe(disposal)).map_err(|_| contract("Facet lifecycle callback panicked"))?;
    std::panic::AssertUnwindSafe(future).catch_unwind().await.map_err(|_| contract("Facet lifecycle future panicked"))?
}
