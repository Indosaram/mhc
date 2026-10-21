use maho_chord::{
    context::BACKGROUND_CONTEXT,
    services::{
        provider::{
            ProviderImplementation, ProviderMember, RemoteServiceProvider,
            ServiceProviderDefinition,
        },
        state::MutableReplicatedStateImpl,
        state_internals::ReplicatedStateInternals,
    },
    types::*,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
fn provider() -> Result<Arc<RemoteServiceProvider>, ServiceError> {
    RemoteServiceProvider::new(vec![ServiceProviderDefinition {
        service_id: "test".into(),
        local: false,
        mode: ServiceMode::Singleton,
    }])
}
fn implementation(
    state: Arc<MutableReplicatedStateImpl>,
) -> Result<Arc<ProviderImplementation>, ServiceError> {
    ProviderImplementation::new(
        Arc::new(()),
        BTreeMap::from([("state".into(), ProviderMember::State(state))]),
    )
}
#[test]
fn running_delivery_is_excluded_from_reentrant_overflow() -> Result<(), ServiceError> {
    let provider = provider()?;
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    provider.provide(&"test".into(), implementation(state.clone())?)?;
    let received = Arc::new(Mutex::new(Vec::new()));
    let output = received.clone();
    let target = state.clone();
    let mut subscription = provider.subscribe(&"test".into(),
        ServiceMode::Singleton,
        Arc::new(move |update, _| {
            let first = matches!(&update, ServiceProviderUpdate::State { sequence, .. } if sequence.number() == 1.0);
            output.lock().map_err(|_| "poisoned")?.push(update);
            if first {
                for value in 3..=103 {
                    target.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?;
                }
            }
            Ok(())
        }),
    )?;
    for value in 1..=2 {
        state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?;
    }
    subscription.activate()?;
    let events = received.lock().map_err(|_| "poisoned")?;
    assert_eq!(events.len(), 3);
    assert!(matches!(
        events[0],
        ServiceProviderUpdate::State { sequence, .. } if sequence.number() == 1.0
    ));
    assert!(matches!(events[1], ServiceProviderUpdate::Reset { .. }));
    assert!(matches!(
        events[2],
        ServiceProviderUpdate::State { sequence, .. } if sequence.number() == 103.0
    ));
    drop(events);
    provider.dispose()?;
    Ok(())
}
#[test]
fn overflow_snapshot_suppresses_already_covered_reentrant_publications() -> Result<(), ServiceError>
{
    let provider = provider()?;
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    let target = Arc::downgrade(&state);
    let remove = state.subscribe_source(Arc::new(move |_, sequence, _| {
        if sequence.number() == 101.0 {
            let state = target.upgrade().ok_or("missing state")?;
            for value in 102..=103 {
                state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?;
            }
        }
        Ok(())
    }));
    provider.provide(&"test".into(), implementation(state.clone())?)?;
    let received = Arc::new(Mutex::new(Vec::new()));
    let output = received.clone();
    let mut subscription = provider.subscribe(&"test".into(),
        ServiceMode::Singleton,
        Arc::new(move |u, _| {
            output.lock().map_err(|_| "poisoned")?.push(u);
            Ok(())
        }),
    )?;
    for value in 1..=101 {
        state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?;
    }
    subscription.activate()?;
    assert_eq!(received.lock().map_err(|_| "poisoned")?.len(), 1);
    if let Some(ServiceProviderUpdate::Reset { snapshot }) =
        received.lock().map_err(|_| "poisoned")?.first()
    {
        assert!(matches!(
            snapshot.instances[0].members[0],
            ServiceMemberSnapshot::State { sequence, .. } if sequence.number() == 103.0
        ));
    } else {
        return Err("expected reset".into());
    }
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":104}).into())?;
    assert_eq!(received.lock().map_err(|_| "poisoned")?.len(), 2);
    remove();
    provider.dispose()?;
    Ok(())
}
#[test]
fn reentrant_disposal_drains_the_terminal_update() -> Result<(), ServiceError> {
    let provider = provider()?;
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    provider.provide(&"test".into(), implementation(state.clone())?)?;
    let target = Arc::downgrade(&provider);
    let received = Arc::new(Mutex::new(Vec::new()));
    let output = received.clone();
    let mut subscription = provider.subscribe(&"test".into(),
        ServiceMode::Singleton,
        Arc::new(move |u, _| {
            let first = matches!(u, ServiceProviderUpdate::State { .. });
            output.lock().map_err(|_| "poisoned")?.push(u);
            if first {
                target.upgrade().ok_or("missing provider")?.dispose()?;
            }
            Ok(())
        }),
    )?;
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into())?;
    subscription.activate()?;
    let events = received.lock().map_err(|_| "poisoned")?;
    assert_eq!(events.len(), 2);
    assert!(matches!(events[1], ServiceProviderUpdate::Unavailable));
    Ok(())
}

#[test]
fn reentrant_lifecycle_transitions_preserve_order_for_every_subscriber() -> Result<(), ServiceError> {
    for dispose in [false, true] {
        let provider = provider()?;
        provider.provide(&"test".into(), implementation(MutableReplicatedStateImpl::new(json!({"value":0}).into())?)?)?;
        let first = Arc::new(Mutex::new(Vec::new())); let output = first.clone(); let target = Arc::downgrade(&provider);
        let mut a = provider.subscribe(&"test".into(), ServiceMode::Singleton, Arc::new(move |update, _| {
            let initial = { let mut events = output.lock().map_err(|_| "poisoned")?; events.push(update); events.len() == 1 };
            if initial { let provider = target.upgrade().ok_or("missing provider")?; if dispose { provider.dispose()?; } else { provider.withdraw(&"test".into())?; } }
            Ok(())
        }))?;
        let second = Arc::new(Mutex::new(Vec::new())); let output = second.clone();
        let mut b = provider.subscribe(&"test".into(), ServiceMode::Singleton, Arc::new(move |update, _| { output.lock().map_err(|_| "poisoned")?.push(update); Ok(()) }))?;
        a.activate()?; b.activate()?;
        provider.replace(&"test".into(), implementation(MutableReplicatedStateImpl::new(json!({"value":1}).into())?)?)?;
        let first = first.lock().map_err(|_| "poisoned")?; let second = second.lock().map_err(|_| "poisoned")?;
        assert_eq!(first.len(), 2); assert_eq!(second.len(), 2);
        assert!(matches!(first[0], ServiceProviderUpdate::Replaced { .. }));
        assert!(matches!(second[0], ServiceProviderUpdate::Replaced { .. }));
        assert!(matches!(first[1], ServiceProviderUpdate::Unavailable));
        assert!(matches!(second[1], ServiceProviderUpdate::Unavailable));
        drop(first); drop(second); provider.dispose()?;
    }
    Ok(())
}
