use maho_chord::{context::BACKGROUND_CONTEXT, delta::Op, services::consumer::RemoteServiceBindingImpl, types::*};
use serde_json::json;
use std::{sync::{Arc, Mutex}, time::Duration};
type Listener = Arc<dyn Fn(ServiceProviderUpdate, ContextRef) + Send + Sync>;
struct Transport { listener: Arc<Mutex<Option<Listener>>> }
struct Subscription { snapshot: ServiceSubscriptionSnapshot }
impl ServiceSubscription for Subscription {
    fn snapshot(&self) -> &ServiceSubscriptionSnapshot { &self.snapshot }
    fn activate(&mut self) -> Result<(), ServiceError> { Ok(()) }
    fn close(&mut self, _: Option<ContextRef>) -> BoxFuture<'_, Result<(), ServiceError>> { Box::pin(async { Ok(()) }) }
}
fn snapshot(value: i32) -> ServiceSubscriptionSnapshot {
    ServiceSubscriptionSnapshot { service_id: "test".into(), mode: ServiceMode::Singleton, instances: vec![ServiceInstanceSnapshot { instance: None, members: vec![ServiceMemberSnapshot::State { name: "state".into(), sequence: seq(0), ops: vec![Op::Replace(json!({"value":value}).into())] }] }] }
}
impl RemoteServiceTransport for Transport {
    fn invoke(&self, _: ServiceCall, _: ContextRef) -> BoxFuture<'_, Result<Option<JsonValue>, ServiceError>> { Box::pin(async { Ok(None) }) }
    fn subscribe(&self, _: JsString, _: ServiceMode, listener: Listener, _: ContextRef) -> BoxFuture<'_, Result<Box<dyn ServiceSubscription>, ServiceError>> {
        *self.listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(listener);
        Box::pin(async { Ok(Box::new(Subscription { snapshot: snapshot(0) }) as Box<dyn ServiceSubscription>) })
    }
}
#[tokio::test]
async fn singleton_reset_rebaselines_sequences_and_can_make_service_unavailable() -> Result<(), ServiceError> {
    let listener = Arc::new(Mutex::new(None));
    let binding = RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: vec!["test".into()], transport: Arc::new(Transport { listener: listener.clone() }), bound: true, on_error: Some(Arc::new(|e| panic!("{e}"))), assert_access: None })?;
    let facade = binding.use_facade(&"test".into())?; let state = facade.member(&"state".into())?;
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    let publish = listener.lock().map_err(|_| "poisoned")?.clone().ok_or("missing listener")?;
    let mut reset = snapshot(101);
    if let ServiceMemberSnapshot::State { sequence, .. } = &mut reset.instances[0].members[0] { *sequence = seq(101); }
    publish(ServiceProviderUpdate::Reset { snapshot: reset }, BACKGROUND_CONTEXT.clone());
    publish(ServiceProviderUpdate::State { instance: None, member: "state".into(), sequence: seq(102), ops: vec![Op::Set(vec!["value".into()], json!(102).into())] }, BACKGROUND_CONTEXT.clone());
    assert_eq!(*state.value()?.ok_or("missing value")?, JsonValue::from(json!({"value":102})));
    let mut unavailable = snapshot(0); unavailable.instances.clear();
    publish(ServiceProviderUpdate::Reset { snapshot: unavailable }, BACKGROUND_CONTEXT.clone());
    assert!(state.value()?.is_none());
    let replacement = snapshot(9).instances.remove(0);
    publish(ServiceProviderUpdate::Replaced { snapshot: replacement }, BACKGROUND_CONTEXT.clone());
    assert_eq!(*state.value()?.ok_or("missing replacement")?, JsonValue::from(json!({"value":9})));
    binding.dispose(BACKGROUND_CONTEXT.clone()).await?; Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
