use maho_chord::{context::BACKGROUND_CONTEXT, services::{provider::{ProviderImplementation, ProviderMember, RemoteServiceProvider, ServiceProviderDefinition}, state::MutableReplicatedStateImpl}, types::*};
use serde_json::json;
use std::{collections::BTreeMap, sync::{Arc, Mutex}};

fn setup() -> Result<(Arc<RemoteServiceProvider>, Arc<MutableReplicatedStateImpl>), ServiceError> {
    let provider = RemoteServiceProvider::new(vec![ServiceProviderDefinition { service_id: "test".into(), local: false, mode: ServiceMode::Singleton }])?;
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    provider.provide(&"test".into(), ProviderImplementation::new(Arc::new(()), BTreeMap::from([("state".into(), ProviderMember::State(state.clone()))]))?)?;
    Ok((provider, state))
}
#[test]
fn failure_in_one_provider_listener_does_not_skip_other_subscribers() -> Result<(), ServiceError> {
    let (provider, state) = setup()?;
    let mut broken = provider.subscribe(&"test".into(), ServiceMode::Singleton, Arc::new(|_, _| Err("listener failed".into())))?;
    broken.activate()?;
    let events = Arc::new(Mutex::new(Vec::new())); let output = events.clone();
    let mut good = provider.subscribe(&"test".into(), ServiceMode::Singleton, Arc::new(move |u, _| { if let ServiceProviderUpdate::State { sequence, .. } = u { output.lock().map_err(|_| "poisoned")?.push(sequence.number() as u64); } Ok(()) }))?;
    good.activate()?;
    assert!(state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into()).is_err());
    assert_eq!(*events.lock().map_err(|_| "poisoned")?, vec![1]);
    drop(broken.close(None));
    provider.dispose()?;
    Ok(())
}
#[test]
fn activation_drains_every_buffered_revision_before_returning_errors() -> Result<(), ServiceError> {
    let (provider, state) = setup()?;
    let events = Arc::new(Mutex::new(Vec::new())); let output = events.clone();
    let mut subscription = provider.subscribe(&"test".into(), ServiceMode::Singleton, Arc::new(move |u, _| { if let ServiceProviderUpdate::State { sequence, .. } = u { output.lock().map_err(|_| "poisoned")?.push(sequence.number() as u64); } Err("listener failed".into()) }))?;
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into())?;
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":2}).into())?;
    assert!(subscription.activate().is_err());
    assert_eq!(*events.lock().map_err(|_| "poisoned")?, vec![1,2]);
    drop(subscription.close(None));
    provider.dispose()?;
    Ok(())
}
#[test]
fn pending_provider_disposal_reaches_the_subscriber_on_activation() -> Result<(), ServiceError> {
    let (provider, state) = setup()?;
    let events = Arc::new(Mutex::new(Vec::new())); let output = events.clone();
    let mut subscription = provider.subscribe(&"test".into(), ServiceMode::Singleton, Arc::new(move |u, _| { output.lock().map_err(|_| "poisoned")?.push(u); Ok(()) }))?;
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into())?;
    provider.dispose()?;
    subscription.activate()?;
    assert_eq!(events.lock().map_err(|_| "poisoned")?.len(), 2);
    assert!(matches!(events.lock().map_err(|_| "poisoned")?.last(), Some(ServiceProviderUpdate::Unavailable)));
    assert!(provider.use_service(&"test".into()).is_err());
    Ok(())
}
#[test]
fn provider_shapes_allowlists_and_modes_are_enforced() -> Result<(), ServiceError> {
    assert!(RemoteServiceProvider::new(vec![ServiceProviderDefinition { service_id: "local".into(), local: true, mode: ServiceMode::Singleton }]).is_err());
    let (provider, state) = setup()?;
    assert!(provider.subscribe(&"unknown".into(), ServiceMode::Singleton, Arc::new(|_, _| Ok(()))).is_err());
    assert!(provider.subscribe(&"test".into(), ServiceMode::Keyed, Arc::new(|_, _| Ok(()))).is_err());
    assert!(provider.provide(&"test".into(), ProviderImplementation::new(Arc::new(()), BTreeMap::from([("state".into(), ProviderMember::State(state))]))?).is_err());
    let incompatible = ProviderImplementation::new(Arc::new(()), BTreeMap::from([("other".into(), ProviderMember::Method(Arc::new(|_, _, _| Box::pin(async { Ok(None) }))))]))?;
    assert!(provider.replace(&"test".into(), incompatible).is_err());
    provider.dispose()?;
    Ok(())
}
