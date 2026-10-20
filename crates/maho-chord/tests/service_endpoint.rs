use maho_chord::{context::BACKGROUND_CONTEXT, services::{provider::*, state::*, wire::*}, types::*};
use maho_chord::services::provider::RemoteServiceProvider;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[tokio::test]
async fn endpoint_owns_control_call_subscriptions_and_cleans_up() -> Result<(), ServiceError> {
    let provider = RemoteServiceProvider::new(vec![ServiceProviderDefinition { service_id: "counter".into(), local: false, mode: ServiceMode::Singleton }])?;
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    provider.provide(&"counter".into(), ProviderImplementation::new(Arc::new(()), BTreeMap::from([("state".into(), ProviderMember::State(state.clone()))]))?)?;
    let endpoint = create_remote_service_endpoint(provider.clone());
    let (events, mut receive) = tokio::sync::mpsc::channel(4);
    let publish: ServiceUpdatePublisher = Arc::new(move |id, update, _| { let events = events.clone(); Box::pin(async move { events.send((id, update)).await.map_err(|e| e.to_string())?; Ok(()) }) });
    let catalogue = endpoint.invoke(create_service_catalogue_call(), publish.clone(), BACKGROUND_CONTEXT.clone()).await?.ok_or("missing catalogue")?;
    assert_eq!(parse_service_catalogue(serde_json::to_value(catalogue)?)?.len(), 1);
    let call = create_service_subscribe_call(&"sub".into(), &"counter".into(), ServiceMode::Singleton);
    let snapshot = endpoint.invoke(call, publish.clone(), BACKGROUND_CONTEXT.clone()).await?.ok_or("missing snapshot")?;
    assert_eq!(parse_service_subscription_snapshot(serde_json::to_value(snapshot)?)?.instances.len(), 1);
    assert!(endpoint.invoke(create_service_subscribe_call(&"sub".into(), &"counter".into(), ServiceMode::Singleton), publish.clone(), BACKGROUND_CONTEXT.clone()).await.is_err());
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into())?;
    let (id, update) = tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing publication")?;
    assert_eq!(id, "sub");
    assert!(matches!(update, ServiceProviderUpdate::State { sequence, .. } if sequence.number() == 1.0));
    endpoint.invoke(create_service_unsubscribe_call(&"sub".into()), publish.clone(), BACKGROUND_CONTEXT.clone()).await?;
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":2}).into())?;
    assert!(matches!(receive.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
    endpoint.dispose().await?;
    assert!(endpoint.invoke(create_service_catalogue_call(), publish, BACKGROUND_CONTEXT.clone()).await.is_err());
    provider.dispose()?;
    Ok(())
}
