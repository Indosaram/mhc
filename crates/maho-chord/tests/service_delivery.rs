use maho_chord::services::provider::RemoteServiceProvider;
use maho_chord::{
    context::BACKGROUND_CONTEXT,
    services::{provider::*, state::*},
    types::*,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[test]
fn provider_activation_appends_reentrant_updates_in_fifo_order() -> Result<(), ServiceError> {
    let provider = RemoteServiceProvider::new(vec![ServiceProviderDefinition {
        service_id: "counter".into(),
        local: false,
        mode: ServiceMode::Singleton,
    }])?;
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    provider.provide(&"counter".into(),
        ProviderImplementation::new(
            Arc::new(()),
            BTreeMap::from([("state".into(), ProviderMember::State(state.clone()))]),
        )?,
    )?;
    let received = Arc::new(Mutex::new(Vec::new()));
    let output = received.clone();
    let target = state.clone();
    let mut subscription = provider.subscribe(&"counter".into(),
        ServiceMode::Singleton,
        Arc::new(move |update, _| {
            if let ServiceProviderUpdate::State { sequence, .. } = update {
                output.lock().map_err(|_| "poisoned")?.push(sequence.number() as u64);
                if sequence.number() == 1.0 {
                    target.replace(BACKGROUND_CONTEXT.clone(), json!({"value":3}).into())?;
                }
            }
            Ok(())
        }),
    )?;
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into())?;
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":2}).into())?;
    subscription.activate()?;
    subscription.activate()?;
    assert_eq!(*received.lock().map_err(|_| "poisoned")?, vec![1, 2, 3]);
    provider.dispose()?;
    Ok(())
}
