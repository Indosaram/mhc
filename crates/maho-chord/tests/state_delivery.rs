use maho_chord::{context::BACKGROUND_CONTEXT, services::state::*, types::*};
use serde_json::json;
use std::{sync::{Arc, Mutex}, time::Duration};

#[tokio::test]
async fn slow_subscriber_is_independent_and_pending_frames_are_bounded() -> Result<(), ServiceError> {
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    let (release, gate) = tokio::sync::oneshot::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(gate)));
    let (events, mut receive) = tokio::sync::mpsc::channel(110);
    let stop = state.subscribe(Box::new(move |_, _, delivery| {
        let gate = if delivery.sequence.number() as u64 == 0 { gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() } else { None };
        let events = events.clone();
        Box::pin(async move { if let Some(gate) = gate { gate.await?; } events.send(delivery.sequence.number() as u64).await.map_err(|e| e.to_string())?; Ok(()) })
    }));
    let fast = Arc::new(Mutex::new(Vec::new())); let output = fast.clone();
    let stop_fast = state.subscribe(Box::new(move |_, _, delivery| { output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(delivery.sequence.number() as u64); Box::pin(async { Ok(()) }) }));
    for value in 1..=202 { state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":value}).into())?; }
    assert_eq!(fast.lock().map_err(|_| "poisoned")?.len(), 203);
    release.send(()).map_err(|_| "gate dropped")?;
    let mut delivered = Vec::new();
    for _ in 0..3 { delivered.push(tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing delivery")?); }
    assert_eq!(delivered, vec![0, 201, 202]);
    stop().await?; stop_fast().await?;
    Ok(())
}
