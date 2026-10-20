use super::{errors::{collected, contract, RemoteServiceError, RemoteServiceErrorCode as Code}, handle::{ServiceObject, ServiceValue}, state::{lock, service_delivery_context}, state_internals::{ReplicatedStateInternals, Unsubscribe}, wire::{decode_service_control_call, ServiceControlCall}};
use crate::{delta::Op, types::*};
use std::{collections::{BTreeMap, VecDeque}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};

pub type UpdateListener = Arc<dyn Fn(ServiceProviderUpdate, ContextRef) -> Result<(), ServiceError> + Send + Sync>;
#[derive(Clone)]
pub enum ProviderMember { Method(super::handle::RemoteMethod), State(Arc<dyn ReplicatedStateInternals>) }
pub struct ProviderImplementation { pub receiver: ServiceImplementation, pub members: BTreeMap<JsString, ProviderMember> }
impl ProviderImplementation {
    pub fn new(receiver: ServiceImplementation, members: BTreeMap<JsString, ProviderMember>) -> Result<Arc<Self>, ServiceError> {
        if members.is_empty() { return Err(contract("Remote service has no members")); }
        Ok(Arc::new(Self { receiver, members }))
    }
}
fn validate_members(implementation: &ProviderImplementation) -> Result<(), ServiceError> {
    if implementation.members.is_empty() { return Err(contract("Remote service has no members")); }
    Ok(())
}
pub fn validate_remote_service_implementation(implementation: &ServiceObject) -> Result<(), ServiceError> {
    if implementation.members.is_empty() { return Err(contract("Remote service has no members")); }
    for value in implementation.members.values() {
        match value { ServiceValue::Method(_) | ServiceValue::State(_) => {}, ServiceValue::Object(_) | ServiceValue::Json(_) => return Err(contract("Remote service member is not remotely exposable")) }
    }
    Ok(())
}
pub struct ServiceProviderDefinition { pub service_id: JsString, pub local: bool, pub mode: ServiceMode }
struct Instance { address: Option<ServiceInstanceAddress>, implementation: Arc<ProviderImplementation>, active: AtomicBool, removers: Mutex<Vec<Unsubscribe>> }
impl Instance { fn stop(&self) { self.active.store(false, Ordering::Release); let removers = std::mem::take(&mut *lock(&self.removers)); for remove in removers { remove(); } } }
type MemberKey = (Option<(JsString, ServiceSequence)>, JsString);
fn member_key(address: &Option<ServiceInstanceAddress>, member: &JsString) -> MemberKey { (address.as_ref().map(|a| (a.key.clone(), a.generation)), member.clone()) }
struct SubscriberData { buffer: VecDeque<(ServiceProviderUpdate, ContextRef)>, sequences: BTreeMap<MemberKey, ServiceSequence>, active: bool, draining: bool, terminated: bool, closed: bool }
struct Subscriber { listener: UpdateListener, data: Mutex<SubscriberData> }
impl Subscriber {
    fn drain(&self) -> Vec<ServiceError> {
        { let mut d = lock(&self.data); if !d.active || d.closed || d.draining { return Vec::new(); } d.draining = true; }
        let mut errors = Vec::new();
        loop {
            let next = { let mut d = lock(&self.data); if d.closed { None } else { d.buffer.pop_front() } };
            let Some((update, context)) = next else { break; };
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.listener)(update, context))) {
                Ok(Ok(())) => {}, Ok(Err(error)) => errors.push(error), Err(_) => errors.push(contract("Service subscriber panicked")),
            }
        }
        let mut d = lock(&self.data); d.draining = false; if d.terminated { d.closed = true; }
        errors
    }
}
struct Registration { mode: ServiceMode, singleton: Option<Arc<Instance>>, shape: Option<BTreeMap<JsString, bool>>, instances: BTreeMap<JsString, Arc<Instance>>, generations: BTreeMap<JsString, ServiceSequence>, subscribers: Vec<Arc<Subscriber>> }
pub struct RemoteServiceProvider { catalogue: Vec<ServiceCatalogueEntry>, registrations: Mutex<BTreeMap<JsString, Registration>>, disposed: AtomicBool }
fn failure(code: Code, message: impl Into<String>) -> ServiceError { Box::new(RemoteServiceError::new(code, message)) }
impl RemoteServiceProvider {
    pub fn new(entries: Vec<ServiceProviderDefinition>) -> Result<Arc<Self>, ServiceError> {
        let mut registrations = BTreeMap::new(); let mut catalogue = Vec::new();
        for entry in entries {
            if entry.local { return Err(contract("Local service cannot be published remotely")); }
            if registrations.contains_key(&entry.service_id) { return Err(contract("Remote service catalogue contains duplicate IDs")); }
            catalogue.push(ServiceCatalogueEntry { service_id: entry.service_id.clone(), mode: entry.mode });
            registrations.insert(entry.service_id, Registration { mode: entry.mode, singleton: None, shape: None, instances: BTreeMap::new(), generations: BTreeMap::new(), subscribers: Vec::new() });
        }
        Ok(Arc::new(Self { catalogue, registrations: Mutex::new(registrations), disposed: AtomicBool::new(false) }))
    }
    pub fn catalogue(&self) -> &[ServiceCatalogueEntry] { &self.catalogue }
    fn active(&self) -> Result<(), ServiceError> { if self.disposed.load(Ordering::Acquire) { Err(contract("Remote service provider is disposed")) } else { Ok(()) } }
    fn registration<'a>(&self, registrations: &'a mut BTreeMap<JsString, Registration>, id: &JsString, mode: ServiceMode) -> Result<&'a mut Registration, ServiceError> {
        self.active()?;
        let r = registrations.get_mut(id).ok_or_else(|| failure(Code::ServiceNotAllowed, "Remote service is not allowlisted"))?;
        if r.mode != mode { return Err(failure(Code::ServiceModeMismatch, "Remote service mode mismatch")); } Ok(r)
    }
    fn shape(implementation: &ProviderImplementation) -> BTreeMap<JsString, bool> { implementation.members.iter().map(|(name, member)| (name.clone(), matches!(member, ProviderMember::State(_)))).collect() }
    fn check_shape(registration: &Registration, implementation: &ProviderImplementation) -> Result<(), ServiceError> {
        if registration.shape.as_ref().is_some_and(|shape| shape != &Self::shape(implementation)) { return Err(failure(Code::ServiceMemberMismatch, "Remote service replacement must preserve its member shape")); } Ok(())
    }
    pub fn validate_replacement(&self, id: &JsString, implementation: &ProviderImplementation) -> Result<(), ServiceError> {
        validate_members(implementation)?;
        let mut registrations = lock(&self.registrations); let r = self.registration(&mut registrations, id, ServiceMode::Singleton)?; Self::check_shape(r, implementation)
    }
    fn create_instance(self: &Arc<Self>, id: &JsString, implementation: Arc<ProviderImplementation>, address: Option<ServiceInstanceAddress>) -> Arc<Instance> {
        let instance = Arc::new(Instance { address: address.clone(), implementation, active: AtomicBool::new(true), removers: Mutex::new(Vec::new()) });
        for (name, member) in &instance.implementation.members {
            if let ProviderMember::State(state) = member {
                let provider = Arc::downgrade(self); let target = Arc::downgrade(&instance); let name = name.clone(); let id = id.to_owned(); let address = address.clone();
                let remove = state.subscribe_source(Arc::new(move |ops, sequence, context| {
                    let (Some(provider), Some(instance)) = (provider.upgrade(), target.upgrade()) else { return Ok(()); };
                    if !instance.active.load(Ordering::Acquire) { return Ok(()); }
                    provider.emit(&id, ServiceProviderUpdate::State { instance: address.clone(), member: name.clone(), sequence, ops: ops.to_vec() }, context)
                }));
                lock(&instance.removers).push(remove);
            }
        }
        instance
    }
    pub fn provide(self: &Arc<Self>, id: &JsString, implementation: Arc<ProviderImplementation>) -> Result<(), ServiceError> {
        validate_members(&implementation)?;
        { let mut rs = lock(&self.registrations); let r = self.registration(&mut rs, id, ServiceMode::Singleton)?; if r.singleton.is_some() { return Err(failure(Code::ServiceModeMismatch, "Remote service already has a provider")); } Self::check_shape(r, &implementation)?; }
        let instance = self.create_instance(id, implementation, None);
        let mut rs = lock(&self.registrations); let r = self.registration(&mut rs, id, ServiceMode::Singleton)?;
        r.shape = Some(Self::shape(&instance.implementation)); r.singleton = Some(instance); Ok(())
    }
    pub fn withdraw(&self, id: &JsString) -> Result<(), ServiceError> {
        let previous = { let mut rs = lock(&self.registrations); self.registration(&mut rs, id, ServiceMode::Singleton)?.singleton.take() };
        if let Some(previous) = previous { previous.stop(); self.emit(id, ServiceProviderUpdate::Unavailable, service_delivery_context())?; } Ok(())
    }
    pub fn replace(self: &Arc<Self>, id: &JsString, implementation: Arc<ProviderImplementation>) -> Result<(), ServiceError> {
        self.validate_replacement(id, &implementation)?;
        let instance = self.create_instance(id, implementation, None);
        let snapshot = snapshot_instance(&instance);
        let previous = { let mut rs = lock(&self.registrations); let r = self.registration(&mut rs, id, ServiceMode::Singleton)?; r.shape = Some(Self::shape(&instance.implementation)); r.singleton.replace(instance) };
        if let Some(previous) = previous { previous.stop(); }
        self.emit(id, ServiceProviderUpdate::Replaced { snapshot }, service_delivery_context())
    }
    pub fn use_service(&self, id: &JsString) -> Result<Arc<ProviderImplementation>, ServiceError> {
        let mut rs = lock(&self.registrations); self.registration(&mut rs, id, ServiceMode::Singleton)?.singleton.as_ref().map(|i| i.implementation.clone()).ok_or_else(|| failure(Code::ServiceNotFound, "Remote service has no local provider"))
    }
    pub fn spawn(self: &Arc<Self>, id: &JsString, key: &JsString, implementation: Arc<ProviderImplementation>) -> Result<Unsubscribe, ServiceError> {
        validate_members(&implementation)?;
        if key.is_empty() { return Err(contract("Remote service instance key must not be empty")); }
        let generation = {
            let mut rs = lock(&self.registrations); let r = self.registration(&mut rs, id, ServiceMode::Keyed)?;
            if r.instances.contains_key(key) { return Err(failure(Code::ServiceModeMismatch, "Remote service already has a live instance with key")); }
            let next = r.generations.get(key).copied().unwrap_or(ServiceSequence::ZERO).next(); r.generations.insert(key.clone(), next); next
        };
        let address = ServiceInstanceAddress { key: key.clone(), generation };
        let instance = self.create_instance(id, implementation, Some(address.clone()));
        { let mut rs = lock(&self.registrations); self.registration(&mut rs, id, ServiceMode::Keyed)?.instances.insert(key.clone(), instance.clone()); }
        self.emit(id, ServiceProviderUpdate::Spawned { instance: snapshot_instance(&instance) }, service_delivery_context())?;
        let provider = Arc::downgrade(self); let id = id.to_owned();
        Ok(Box::new(move || { instance.stop(); if let Some(provider) = provider.upgrade() {
            let removed = { let mut rs = lock(&provider.registrations); rs.get_mut(&id).is_some_and(|r| { if r.instances.get(&address.key).is_some_and(|i| Arc::ptr_eq(i, &instance)) { r.instances.remove(&address.key); true } else { false } }) };
            if removed { if let Err(error) = provider.emit(&id, ServiceProviderUpdate::Closed { instance: address }, service_delivery_context()) { std::panic::panic_any(error); } }
        } }))
    }
    pub async fn invoke(&self, call: ServiceCall, context: ContextRef) -> Result<Option<JsonValue>, ServiceError> {
        self.active()?;
        let instance = {
            let rs = lock(&self.registrations); let r = rs.get(&call.service_id).ok_or_else(|| failure(Code::ServiceNotAllowed, "Remote service is not allowlisted"))?;
            match (&r.mode, &call.instance) {
                (ServiceMode::Singleton, None) => r.singleton.clone().ok_or_else(|| failure(Code::ServiceNotFound, "Remote service has no provider"))?,
                (ServiceMode::Keyed, Some(address)) => {
                    let instance = r.instances.get(&address.key).ok_or_else(|| failure(Code::ServiceInstanceNotFound, "Remote service has no instance"))?;
                    if instance.address.as_ref().map(|a| a.generation) != Some(address.generation) { return Err(failure(Code::ServiceStaleInstance, "Remote service instance is stale")); } instance.clone()
                }
                (ServiceMode::Singleton, Some(_)) | (ServiceMode::Keyed, None) => return Err(failure(Code::ServiceModeMismatch, "Remote service mode mismatch")),
            }
        };
        let member = instance.implementation.members.get(&call.member).ok_or_else(|| failure(Code::ServiceMemberNotFound, "Unknown remote service member"))?;
        match member { ProviderMember::Method(method) => method(instance.implementation.receiver.clone(), call.args, context).await, ProviderMember::State(_) => Err(failure(Code::ServiceMemberMismatch, "Remote service member is not a method")) }
    }
    pub fn subscribe(&self, id: &JsString, mode: ServiceMode, listener: UpdateListener) -> Result<ProviderSubscription, ServiceError> {
        let mut rs = lock(&self.registrations); let r = self.registration(&mut rs, id, mode)?;
        if mode == ServiceMode::Singleton && r.singleton.is_none() { return Err(failure(Code::ServiceNotFound, "Remote service has no provider")); }
        let snapshot = snapshot_registration(id, r); let mut sequences = BTreeMap::new(); record_sequences(&mut sequences, &snapshot.instances);
        let subscriber = Arc::new(Subscriber { listener, data: Mutex::new(SubscriberData { buffer: VecDeque::new(), sequences, active: false, draining: false, terminated: false, closed: false }) });
        r.subscribers.push(subscriber.clone());
        Ok(ProviderSubscription { snapshot, subscriber })
    }
    fn emit(&self, id: &JsString, update: ServiceProviderUpdate, context: ContextRef) -> Result<(), ServiceError> {
        let (subscribers, snapshot) = {
            let mut rs = lock(&self.registrations); let Some(r) = rs.get_mut(id) else { return Ok(()); };
            r.subscribers.retain(|s| !lock(&s.data).closed);
            (r.subscribers.clone(), snapshot_registration(id, r))
        };
        for subscriber in &subscribers {
            let mut d = lock(&subscriber.data);
            if d.closed || covered(&mut d.sequences, &update) { continue; }
            if d.buffer.len() == 100 { d.buffer.clear(); d.sequences.clear(); record_sequences(&mut d.sequences, &snapshot.instances); d.buffer.push_back((ServiceProviderUpdate::Reset { snapshot: snapshot.clone() }, context.clone())); }
            else { d.buffer.push_back((update.clone(), context.clone())); }
        }
        collected(subscribers.iter().flat_map(|s| s.drain()).collect(), "Failed to publish remote service update")
    }
    pub fn dispose(&self) -> Result<(), ServiceError> {
        if self.disposed.swap(true, Ordering::AcqRel) { return Ok(()); }
        let ids = lock(&self.registrations).keys().cloned().collect::<Vec<_>>(); let mut errors = Vec::new();
        for id in ids {
            let instances = { let mut rs = lock(&self.registrations); let Some(r) = rs.get_mut(&id) else { continue; }; let mut instances = r.singleton.take().into_iter().collect::<Vec<_>>(); instances.extend(std::mem::take(&mut r.instances).into_values()); instances };
            for instance in instances { instance.stop(); let update = match &instance.address { None => ServiceProviderUpdate::Unavailable, Some(address) => ServiceProviderUpdate::Closed { instance: address.clone() } }; if let Err(error) = self.emit(&id, update, service_delivery_context()) { errors.push(error); } }
            if let Some(r) = lock(&self.registrations).get_mut(&id) { for subscriber in &r.subscribers { let mut d = lock(&subscriber.data); if d.active && !d.draining { d.closed = true; d.buffer.clear(); } else { d.terminated = true; } } }
        }
        lock(&self.registrations).clear(); collected(errors, "Failed to dispose remote service provider")
    }
}
fn snapshot_instance(instance: &Instance) -> ServiceInstanceSnapshot {
    ServiceInstanceSnapshot { instance: instance.address.clone(), members: instance.implementation.members.iter().map(|(name, member)| match member {
        ProviderMember::Method(_) => ServiceMemberSnapshot::Method { name: name.clone() },
        ProviderMember::State(state) => { let (value, sequence) = state.snapshot(); ServiceMemberSnapshot::State { name: name.clone(), sequence, ops: vec![Op::Replace((*value).clone())] } }
    }).collect() }
}
fn snapshot_registration(id: &JsString, r: &Registration) -> ServiceSubscriptionSnapshot { ServiceSubscriptionSnapshot { service_id: id.clone(), mode: r.mode, instances: match r.mode { ServiceMode::Singleton => r.singleton.iter().map(|i| snapshot_instance(i)).collect(), ServiceMode::Keyed => r.instances.values().map(|i| snapshot_instance(i)).collect() } } }
fn record_sequences(sequences: &mut BTreeMap<MemberKey, ServiceSequence>, instances: &[ServiceInstanceSnapshot]) { for instance in instances { for member in &instance.members { if let ServiceMemberSnapshot::State { name, sequence, .. } = member { sequences.insert(member_key(&instance.instance, name), *sequence); } } } }
fn covered(sequences: &mut BTreeMap<MemberKey, ServiceSequence>, update: &ServiceProviderUpdate) -> bool {
    match update {
        ServiceProviderUpdate::State { instance, member, sequence, .. } => { let key = member_key(instance, member); if sequences.get(&key).is_some_and(|n| sequence <= n) { return true; } sequences.remove(&key); }
        ServiceProviderUpdate::Reset { snapshot } => { sequences.clear(); record_sequences(sequences, &snapshot.instances); }
        ServiceProviderUpdate::Replaced { snapshot } => { sequences.clear(); record_sequences(sequences, std::slice::from_ref(snapshot)); }
        ServiceProviderUpdate::Spawned { instance } => record_sequences(sequences, std::slice::from_ref(instance)),
        ServiceProviderUpdate::Unavailable => sequences.clear(),
        ServiceProviderUpdate::Closed { instance } => sequences.retain(|(address, _), _| address.as_ref().is_none_or(|(key, generation)| key != &instance.key || *generation != instance.generation)),
    } false
}
pub struct ProviderSubscription { snapshot: ServiceSubscriptionSnapshot, subscriber: Arc<Subscriber> }
impl RemoteServiceTransport for RemoteServiceProvider {
    fn invoke(&self, call: ServiceCall, context: ContextRef) -> BoxFuture<'_, Result<Option<JsonValue>, ServiceError>> {
        Box::pin(RemoteServiceProvider::invoke(self, call, context))
    }
    fn subscribe(&self, service_id: JsString, mode: ServiceMode, listener: Arc<dyn Fn(ServiceProviderUpdate, ContextRef) + Send + Sync>, _: ContextRef) -> BoxFuture<'_, Result<Box<dyn ServiceSubscription>, ServiceError>> {
        Box::pin(async move {
            let subscription = RemoteServiceProvider::subscribe(self, &service_id, mode, Arc::new(move |update, context| { listener(update, context); Ok(()) }))?;
            Ok(Box::new(subscription) as Box<dyn ServiceSubscription>)
        })
    }
}
impl crate::types::RemoteServiceProvider for RemoteServiceProvider {
    fn catalogue(&self) -> Vec<ServiceCatalogueEntry> { self.catalogue.clone() }
}
impl ServiceSubscription for ProviderSubscription {
    fn snapshot(&self) -> &ServiceSubscriptionSnapshot { &self.snapshot }
    fn activate(&mut self) -> Result<(), ServiceError> { { let mut d = lock(&self.subscriber.data); if d.closed || d.active { return Ok(()); } d.active = true; } collected(self.subscriber.drain(), "Failed to activate remote service subscription") }
    fn close(&mut self, _: Option<ContextRef>) -> BoxFuture<'_, Result<(), ServiceError>> { let mut d = lock(&self.subscriber.data); d.closed = true; d.buffer.clear(); Box::pin(async { Ok(()) }) }
}
pub type ServiceUpdatePublisher = Arc<dyn Fn(JsString, ServiceProviderUpdate, ContextRef) -> BoxFuture<'static, Result<(), ServiceError>> + Send + Sync>;
pub struct RemoteServiceEndpoint { provider: Arc<RemoteServiceProvider>, subscriptions: tokio::sync::Mutex<BTreeMap<JsString, ProviderSubscription>>, disposed: AtomicBool }
pub fn create_remote_service_endpoint(provider: Arc<RemoteServiceProvider>) -> RemoteServiceEndpoint { RemoteServiceEndpoint { provider, subscriptions: tokio::sync::Mutex::new(BTreeMap::new()), disposed: AtomicBool::new(false) } }
impl RemoteServiceEndpoint {
    pub async fn invoke(&self, call: ServiceCall, publish: ServiceUpdatePublisher, context: ContextRef) -> Result<Option<JsonValue>, ServiceError> {
        if self.disposed.load(Ordering::Acquire) { return Err(contract("Remote service endpoint is disposed")); }
        match decode_service_control_call(&call) {
            Some(ServiceControlCall::Catalogue) => Ok(Some(serde_json::from_str(&serde_json::to_string(self.provider.catalogue())?)?)),
            Some(ServiceControlCall::Subscribe { subscription_id, service_id, mode }) => {
                let mut subscriptions = self.subscriptions.lock().await;
                if subscriptions.contains_key(&subscription_id) { return Err(contract("Service subscription ID is already active")); }
                let id = subscription_id.clone();
                let mut subscription = self.provider.subscribe(&service_id, mode, Arc::new(move |update, context| {
                    let future = publish(id.clone(), update, context);
                    tokio::spawn(async move { if let Err(error) = future.await { std::panic::panic_any(error); } }); Ok(())
                }))?;
                let snapshot = serde_json::from_str(&serde_json::to_string(subscription.snapshot())?)?;
                subscription.activate()?; subscriptions.insert(subscription_id, subscription); Ok(Some(snapshot))
            }
            Some(ServiceControlCall::Unsubscribe { subscription_id }) => {
                let mut subscription = self.subscriptions.lock().await.remove(&subscription_id).ok_or_else(|| contract("Service subscription was not found"))?;
                subscription.close(Some(context)).await?; Ok(None)
            }
            None => self.provider.invoke(call, context).await,
        }
    }
    pub async fn dispose(&self) -> Result<(), ServiceError> {
        if self.disposed.swap(true, Ordering::AcqRel) { return Ok(()); }
        let subscriptions = std::mem::take(&mut *self.subscriptions.lock().await); let mut errors = Vec::new();
        for mut subscription in subscriptions.into_values() { if let Err(error) = subscription.close(None).await { errors.push(error); } }
        collected(errors, "Failed to dispose remote service endpoint")
    }
}
