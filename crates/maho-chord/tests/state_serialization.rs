use maho_chord::{context::BACKGROUND_CONTEXT, delta::Op, services::state::ReplicatedStateReplica, types::*};
use serde_json::json;
use std::{sync::{Arc, Mutex}, time::Duration};

#[tokio::test]
async fn disconnect_drops_obsolete_pending_work_but_waits_for_running_callback() -> Result<(), ServiceError> {
    let replica = ReplicatedStateReplica::new(Arc::new(|error| panic!("{error}")));
    let (release, gate) = tokio::sync::oneshot::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(gate)));
    let (events, mut receive) = tokio::sync::mpsc::channel(4);
    let stop = replica.subscribe(Box::new(move |_, _, delivery| {
        let gate = if delivery.sequence.number() as u64 == 0 { gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() } else { None };
        let events = events.clone(); Box::pin(async move { if let Some(gate) = gate { gate.await?; } events.send(delivery).await.map_err(|e| e.to_string())?; Ok(()) })
    }));
    replica.hydrate(seq(0), &[Op::Replace(json!({"value":0}).into())], BACKGROUND_CONTEXT.clone())?;
    replica.update(seq(1), &[Op::Set(vec!["value".into()], json!(1).into())], BACKGROUND_CONTEXT.clone())?;
    replica.clear();
    replica.hydrate(seq(50), &[Op::Replace(json!({"value":50}).into())], BACKGROUND_CONTEXT.clone())?;
    replica.update(seq(51), &[Op::Set(vec!["value".into()], json!(51).into())], BACKGROUND_CONTEXT.clone())?;
    release.send(()).map_err(|_| "gate dropped")?;
    let mut delivered = Vec::new();
    for _ in 0..3 { delivered.push(tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing delivery")?); }
    assert_eq!(delivered, vec![ReplicatedStateDelivery { kind: ReplicatedStateDeliveryKind::Hydrate, sequence: seq(0) }, ReplicatedStateDelivery { kind: ReplicatedStateDeliveryKind::Hydrate, sequence: seq(50) }, ReplicatedStateDelivery { kind: ReplicatedStateDeliveryKind::Update, sequence: seq(51) }]);
    stop().await?; Ok(())
}
#[tokio::test]
async fn rejection_after_unsubscribe_is_reported_without_replaying_pending_frames() -> Result<(), ServiceError> {
    let (reports, mut reported) = tokio::sync::mpsc::channel(2);
    let replica = ReplicatedStateReplica::new(Arc::new(move |error| { if let Err(error) = reports.try_send(error.to_string()) { panic!("{error}"); } }));
    let (release, gate) = tokio::sync::oneshot::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(gate))); let callbacks = Arc::new(Mutex::new(Vec::new())); let output = callbacks.clone();
    let stop = replica.subscribe(Box::new(move |_, _, delivery| {
        output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(delivery.sequence.number() as u64);
        let gate = gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        Box::pin(async move { if let Some(gate) = gate { gate.await?; } Err("stopped callback".into()) })
    }));
    replica.hydrate(seq(0), &[Op::Replace(json!({"value":0}).into())], BACKGROUND_CONTEXT.clone())?;
    replica.update(seq(1), &[Op::Set(vec!["value".into()], json!(1).into())], BACKGROUND_CONTEXT.clone())?;
    stop().await?;
    release.send(()).map_err(|_| "gate dropped")?;
    assert_eq!(tokio::time::timeout(Duration::from_secs(1), reported.recv()).await?.ok_or("missing report")?, "stopped callback");
    assert_eq!(*callbacks.lock().map_err(|_| "poisoned")?, vec![0]); Ok(())
}
#[test]
fn hydration_and_reentrant_updates_are_serialized_for_all_cold_listeners() -> Result<(), ServiceError> {
    let replica = ReplicatedStateReplica::new(Arc::new(|error| panic!("{error}")));
    let events = Arc::new(Mutex::new(Vec::new())); let mut stops = Vec::new();
    for index in 0..2 {
        let target = Arc::downgrade(&replica); let output = events.clone();
        stops.push(replica.subscribe(Box::new(move |_, _, delivery| {
            output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((index, delivery.sequence.number() as u64));
            if index == 0 && delivery.sequence.number() as u64 == 0 {
                let result = target.upgrade().ok_or("missing replica").map_err(ServiceError::from).and_then(|r| r.update(seq(1), &[Op::Set(vec!["value".into()], json!(1).into())], BACKGROUND_CONTEXT.clone()));
                return Box::pin(async move { result });
            } Box::pin(async { Ok(()) })
        })));
    }
    replica.hydrate(seq(0), &[Op::Replace(json!({"value":0}).into())], BACKGROUND_CONTEXT.clone())?;
    let events = events.lock().map_err(|_| "poisoned")?;
    for index in 0..2 { assert_eq!(events.iter().filter(|(i, _)| *i == index).map(|(_,s)| *s).collect::<Vec<_>>(), vec![0,1]); }
    drop(stops); Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
