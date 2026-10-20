use maho_chord::{context::BACKGROUND_CONTEXT, services::{consumer::RemoteServiceBindingImpl, provider::{RemoteServiceProvider, ProviderImplementation, ProviderMember, ServiceProviderDefinition}, state::MutableReplicatedStateImpl, state_codec::*, wire::*}, types::*};
use serde_json::json;
use std::{collections::BTreeMap, sync::{Arc, Mutex}, time::Duration};
type Listener = Arc<dyn Fn(ServiceProviderUpdate, ContextRef) + Send + Sync>;
struct Transport { provider: Arc<RemoteServiceProvider>, before_activate: Arc<dyn Fn() -> Result<(), ServiceError> + Send + Sync> }
struct Subscription { snapshot: ServiceSubscriptionSnapshot, inner: Box<dyn ServiceSubscription>, before_activate: Arc<dyn Fn() -> Result<(), ServiceError> + Send + Sync> }
impl ServiceSubscription for Subscription {
    fn snapshot(&self) -> &ServiceSubscriptionSnapshot { &self.snapshot }
    fn activate(&mut self) -> Result<(), ServiceError> { (self.before_activate)()?; self.inner.activate() }
    fn close(&mut self, context: Option<ContextRef>) -> BoxFuture<'_, Result<(), ServiceError>> { self.inner.close(context) }
}
impl RemoteServiceTransport for Transport {
    fn invoke(&self, call: ServiceCall, context: ContextRef) -> BoxFuture<'_, Result<Option<JsonValue>, ServiceError>> { Box::pin(self.provider.invoke(call, context)) }
    fn subscribe(&self, id: JsString, mode: ServiceMode, listener: Listener, _: ContextRef) -> BoxFuture<'_, Result<Box<dyn ServiceSubscription>, ServiceError>> {
        Box::pin(async move {
            let encoder = Arc::new(Mutex::new(create_service_state_encoder())); let decoder = Arc::new(Mutex::new(create_service_state_decoder()));
            let enc = encoder.clone(); let dec = decoder.clone();
            let subscription = self.provider.subscribe(&id, mode, Arc::new(move |update, context| {
                let encoded = enc.lock().map_err(|_| "poisoned")?.encode_update(update)?;
                let decoded = dec.lock().map_err(|_| "poisoned")?.decode_update(parse_wire_service_provider_update(serde_json::to_value(encoded)?)?)?;
                listener(decoded, context); Ok(())
            }))?;
            let encoded = encoder.lock().map_err(|_| "poisoned")?.encode_snapshot(subscription.snapshot().clone())?;
            let snapshot = decoder.lock().map_err(|_| "poisoned")?.decode_snapshot(parse_wire_service_subscription_snapshot(serde_json::to_value(encoded)?)?)?;
            Ok(Box::new(Subscription { snapshot, inner: Box::new(subscription), before_activate: self.before_activate.clone() }) as Box<dyn ServiceSubscription>)
        })
    }
}
fn state(value: i32) -> Result<Arc<MutableReplicatedStateImpl>, ServiceError> { MutableReplicatedStateImpl::new(json!({"value":value}).into()) }
fn implementation(members: Vec<(&str, Arc<MutableReplicatedStateImpl>)>) -> Result<Arc<ProviderImplementation>, ServiceError> {
    ProviderImplementation::new(Arc::new(()), members.into_iter().map(|(name, state)| (name.into(), ProviderMember::State(state))).collect::<BTreeMap<_,_>>())
}
fn binding(provider: Arc<RemoteServiceProvider>, action: Arc<dyn Fn() -> Result<(), ServiceError> + Send + Sync>) -> Result<Arc<RemoteServiceBindingImpl>, ServiceError> {
    RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: vec!["test".into()], transport: Arc::new(Transport { provider, before_activate: action }), bound: true, on_error: Some(Arc::new(|e| panic!("{e}"))), assert_access: None })
}
#[tokio::test]
async fn overflow_rebaselines_all_members_through_wire_then_continues_deltas() -> Result<(), ServiceError> {
    let provider = RemoteServiceProvider::new(vec![ServiceProviderDefinition { service_id: "test".into(), local: false, mode: ServiceMode::Singleton }])?;
    let left = state(0)?; let right = state(0)?;
    provider.provide(&"test".into(), implementation(vec![("left",left.clone()),("right",right.clone())])?)?;
    let l = left.clone(); let r = right.clone();
    let binding = binding(provider.clone(), Arc::new(move || { for value in 1..=101 { l.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?; r.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?; } Ok(()) }))?;
    let facade = binding.use_facade(&"test".into())?;
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    for member in ["left","right"] { assert_eq!(*facade.member(&member.into())?.value()?.ok_or("missing state")?, JsonValue::from(json!({"value":101}))); }
    for value in 102..=104 { left.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?; right.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?; }
    for member in ["left","right"] { assert_eq!(*facade.member(&member.into())?.value()?.ok_or("missing state")?, JsonValue::from(json!({"value":104}))); }
    binding.dispose(BACKGROUND_CONTEXT.clone()).await?; provider.dispose()?; Ok(())
}
#[tokio::test]
async fn overflow_reset_can_make_singleton_unavailable_before_replacement() -> Result<(), ServiceError> {
    let provider = RemoteServiceProvider::new(vec![ServiceProviderDefinition { service_id: "test".into(), local: false, mode: ServiceMode::Singleton }])?;
    provider.provide(&"test".into(), implementation(vec![("state",state(0)?)])?)?;
    let target = provider.clone();
    let binding = binding(provider.clone(), Arc::new(move || { for value in 1..=100 { target.replace(&"test".into(), implementation(vec![("state",state(value)?)])?)?; } target.withdraw(&"test".into()) }))?;
    let facade = binding.use_facade(&"test".into())?;
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    assert!(facade.member(&"state".into())?.value()?.is_none());
    provider.replace(&"test".into(), implementation(vec![("state",state(200)?)])?)?;
    assert_eq!(*facade.member(&"state".into())?.value()?.ok_or("missing replacement")?, JsonValue::from(json!({"value":200})));
    binding.dispose(BACKGROUND_CONTEXT.clone()).await?; provider.dispose()?; Ok(())
}
#[tokio::test]
async fn keyed_overflow_reset_retains_live_generations_and_reconciles_reused_keys() -> Result<(), ServiceError> {
    let provider = RemoteServiceProvider::new(vec![ServiceProviderDefinition { service_id: "test".into(), local: false, mode: ServiceMode::Keyed }])?;
    let retained = state(0)?;
    let close_a = provider.spawn(&"test".into(), &"a".into(), implementation(vec![("state",retained.clone())])?)?;
    let close_b = provider.spawn(&"test".into(), &"b".into(), implementation(vec![("state",state(0)?)])?)?;
    let target = retained.clone();
    let binding = binding(provider.clone(), Arc::new(move || { for value in 1..=101 { target.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?; } Ok(()) }))?;
    let (events, mut receive) = tokio::sync::mpsc::channel(8);
    let stop = binding.observe(&"test".into(), Arc::new(move |service, context| { let events = events.clone(); Box::pin(async move { events.send((service,context)).await.map_err(|e| e.to_string())?; Ok(()) }) }))?;
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    let (a, a_context) = tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing a")?;
    let (b, b_context) = tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing b")?;
    let a = a.downcast::<maho_chord::services::consumer::ServiceFacade>().map_err(|_| "wrong facade")?;
    assert_eq!(*a.member(&"state".into())?.value()?.ok_or("missing state")?, JsonValue::from(json!({"value":101})));
    assert!(!a_context.abort_signal().ok_or("missing a signal")?.aborted());
    close_b();
    assert!(b_context.abort_signal().ok_or("missing b signal")?.aborted());
    let close_reused = provider.spawn(&"test".into(), &"b".into(), implementation(vec![("state",state(9)?)])?)?;
    let (reused, _) = tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing reused b")?;
    assert!(!Arc::ptr_eq(&b, &reused));
    retained.replace(BACKGROUND_CONTEXT.clone(), json!({"value":102}).into())?;
    assert_eq!(*a.member(&"state".into())?.value()?.ok_or("missing updated state")?, JsonValue::from(json!({"value":102})));
    close_a(); close_reused(); stop().await?; binding.dispose(BACKGROUND_CONTEXT.clone()).await?; provider.dispose()?; Ok(())
}
