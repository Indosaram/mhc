use maho_chord::{context::BACKGROUND_CONTEXT, services::{consumer::*, provider::*, loopback::*, state::*}, types::*};
use serde_json::json;
use std::{collections::BTreeMap, sync::{Arc, Mutex}, time::Duration};
use maho_chord::services::provider::RemoteServiceProvider;

fn implementation(state: Arc<MutableReplicatedStateImpl>) -> Result<Arc<ProviderImplementation>, ServiceError> {
    ProviderImplementation::new(Arc::new(()), BTreeMap::from([
        ("state".into(), ProviderMember::State(state)),
        ("echo".into(), ProviderMember::Method(Arc::new(|_, args, _| Box::pin(async move { Ok(args.into_iter().next()) })))),
    ]))
}
fn provider(mode: ServiceMode) -> Result<Arc<RemoteServiceProvider>, ServiceError> {
    RemoteServiceProvider::new(vec![ServiceProviderDefinition { service_id: "test".into(), local: false, mode }])
}
fn binding(provider: Arc<RemoteServiceProvider>) -> Result<Arc<RemoteServiceBindingImpl>, ServiceError> {
    RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: vec!["test".into()], transport: create_loopback_service_transport(provider), bound: true, on_error: Some(Arc::new(|error| panic!("{error}"))), assert_access: None })
}
#[tokio::test]
async fn singleton_facade_survives_withdrawal_replacement_and_rebind() -> Result<(), ServiceError> {
    let provider = provider(ServiceMode::Singleton)?;
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    provider.provide(&"test".into(), implementation(state.clone())?)?;
    let binding = binding(provider.clone())?;
    let facade = binding.use_facade(&"test".into())?;
    let member = facade.member(&"state".into())?;
    let method = facade.member(&"echo".into())?;
    assert!(member.value()?.is_none());
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    assert_eq!(*member.value()?.ok_or("missing value")?, JsonValue::from(json!({"value":0})));
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into())?;
    assert_eq!(*member.value()?.ok_or("missing value")?, JsonValue::from(json!({"value":1})));
    assert_eq!(method.invoke(vec![json!({"hello":"world"}).into()], BACKGROUND_CONTEXT.clone()).await?, Some(json!({"hello":"world"}).into()));
    provider.withdraw(&"test".into())?;
    assert!(member.value()?.is_none());
    let replacement = MutableReplicatedStateImpl::new(json!({"value":9}).into())?;
    provider.replace(&"test".into(), implementation(replacement.clone())?)?;
    assert!(Arc::ptr_eq(&facade, &binding.use_facade(&"test".into())?));
    assert!(Arc::ptr_eq(&member, &facade.member(&"state".into())?));
    assert_eq!(*member.value()?.ok_or("missing value")?, JsonValue::from(json!({"value":9})));
    binding.rebind(false, BACKGROUND_CONTEXT.clone()).await?;
    assert!(member.value()?.is_none());
    assert!(method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    tokio::time::timeout(Duration::from_secs(1), binding.rebind(true, BACKGROUND_CONTEXT.clone())).await??;
    assert_eq!(*member.value()?.ok_or("missing value")?, JsonValue::from(json!({"value":9})));
    binding.dispose(BACKGROUND_CONTEXT.clone()).await?;
    assert!(member.value().is_err());
    provider.dispose()?;
    Ok(())
}
#[tokio::test]
async fn keyed_instances_cancel_observers_and_reject_old_generations() -> Result<(), ServiceError> {
    let provider = provider(ServiceMode::Keyed)?;
    let binding = binding(provider.clone())?;
    let (events, mut receive) = tokio::sync::mpsc::channel(4);
    let stop = binding.observe(&"test".into(), Arc::new(move |service, context| {
        let events = events.clone();
        Box::pin(async move { events.send((service, context)).await.map_err(|e| e.to_string())?; Ok(()) })
    }))?;
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    let close = provider.spawn(&"test".into(), &"dialog".into(), implementation(MutableReplicatedStateImpl::new(json!({"value":1}).into())?)?)?;
    let (old, context) = tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing observation")?;
    let old = old.downcast::<ServiceFacade>().map_err(|_| "wrong facade")?;
    let signal = context.abort_signal().ok_or("missing observation signal")?;
    let cancellation = signal.cancelled();
    close();
    tokio::time::timeout(Duration::from_secs(1), cancellation).await?;
    assert!(old.member(&"echo".into())?.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    let close = provider.spawn(&"test".into(), &"dialog".into(), implementation(MutableReplicatedStateImpl::new(json!({"value":2}).into())?)?)?;
    let (new, _) = tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing second observation")?;
    assert!(!Arc::ptr_eq(&old, &new.downcast::<ServiceFacade>().map_err(|_| "wrong facade")?));
    assert!(provider.invoke(ServiceCall { service_id: "test".into(), instance: Some(ServiceInstanceAddress { key: "dialog".into(), generation: seq(1) }), member: "echo".into(), args: vec![] }, BACKGROUND_CONTEXT.clone()).await.is_err());
    close(); stop().await?; binding.dispose(BACKGROUND_CONTEXT.clone()).await?; provider.dispose()?;
    Ok(())
}
#[test]
fn provider_buffers_overflow_as_reset_and_then_contiguous_updates() -> Result<(), ServiceError> {
    for count in [100,101,102,201,202] {
        let provider = provider(ServiceMode::Singleton)?;
        let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
        provider.provide(&"test".into(), implementation(state.clone())?)?;
        let updates = Arc::new(Mutex::new(Vec::new())); let output = updates.clone();
        let mut subscription = provider.subscribe(&"test".into(), ServiceMode::Singleton, Arc::new(move |u,_| { output.lock().map_err(|_| "poisoned")?.push(u); Ok(()) }))?;
        for value in 1..=count { state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?; }
        assert!(updates.lock().map_err(|_| "poisoned")?.is_empty());
        subscription.activate()?;
        let first = ((count - 1) / 100) * 100 + 1;
        assert_eq!(updates.lock().map_err(|_| "poisoned")?.len(), usize::try_from(count - first + 1)?);
        if count > 100 { assert!(matches!(updates.lock().map_err(|_| "poisoned")?.first(), Some(ServiceProviderUpdate::Reset { .. }))); }
        state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":count+1}).into())?;
        assert!(matches!(updates.lock().map_err(|_| "poisoned")?.last(), Some(ServiceProviderUpdate::State { sequence, .. }) if *sequence == seq(u64::try_from(count+1)?)));
        provider.dispose()?;
    }
    Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }

#[tokio::test]
async fn native_provider_binding_and_member_maps_preserve_surrogate_identifiers() -> Result<(), ServiceError> {
    let id = JsString::from_utf16(vec![0xd800]);
    let member = JsString::from_utf16(vec![0xdc00]);
    let key = JsString::from_utf16(vec![0xdfff]);
    let provider = RemoteServiceProvider::new(vec![ServiceProviderDefinition { service_id: id.clone(), local: false, mode: ServiceMode::Keyed }])?;
    let binding = RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: vec![id.clone()], transport: create_loopback_service_transport(provider.clone()), bound: true, on_error: Some(Arc::new(|error| panic!("{error}"))), assert_access: None })?;
    let (sent, mut received) = tokio::sync::mpsc::channel(1);
    let stop = binding.observe(&id, Arc::new(move |service, _| { let sent = sent.clone(); Box::pin(async move { sent.send(service).await.map_err(|error| error.to_string())?; Ok(()) }) }))?;
    binding.ready(BACKGROUND_CONTEXT.clone()).await?;
    let implementation = ProviderImplementation::new(Arc::new(()), BTreeMap::from([(member.clone(), ProviderMember::Method(Arc::new(|_, args, _| Box::pin(async move { Ok(args.into_iter().next()) }))))]))?;
    let close = provider.spawn(&id, &key, implementation)?;
    let facade = tokio::time::timeout(Duration::from_secs(1), received.recv()).await?.ok_or("missing surrogate instance")?.downcast::<ServiceFacade>().map_err(|_| "wrong facade")?;
    let argument = JsonValue::String(key);
    assert_eq!(facade.member(&member)?.invoke(vec![argument.clone()], BACKGROUND_CONTEXT.clone()).await?, Some(argument));
    close(); stop().await?; binding.dispose(BACKGROUND_CONTEXT.clone()).await?; provider.dispose()?;
    Ok(())
}
