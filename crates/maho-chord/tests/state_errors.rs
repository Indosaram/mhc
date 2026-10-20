use maho_chord::{context::BACKGROUND_CONTEXT, delta::Op, services::state::ReplicatedStateReplica, types::*};
use serde_json::json;
use std::{sync::{Arc, Mutex}, time::Duration};

#[tokio::test]
async fn hydration_errors_and_update_errors_are_reported_without_removing_subscription() -> Result<(), ServiceError> {
    let (reports, mut receive) = tokio::sync::mpsc::channel(8);
    let replica = ReplicatedStateReplica::new(Arc::new(move |error| { if let Err(error) = reports.try_send(error.to_string()) { panic!("{error}"); } }));
    let seen = Arc::new(Mutex::new(Vec::new())); let output = seen.clone();
    let stop = replica.subscribe(Box::new(move |_,_,delivery| {
        output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(delivery.sequence.number() as u64);
        if delivery.sequence.number() as u64 == 1 { panic!("sync callback failure"); }
        Box::pin(async move { if delivery.sequence.number() as u64 == 0 || delivery.sequence.number() as u64 == 2 { Err("callback rejection".into()) } else { Ok(()) } })
    }));
    replica.hydrate(seq(0), &[Op::Replace(json!({"value":0}).into())], BACKGROUND_CONTEXT.clone())?;
    for sequence in 1..=3 { replica.update(seq(sequence), &[Op::Set(vec!["value".into()], json!(sequence).into())], BACKGROUND_CONTEXT.clone())?; }
    for _ in 0..3 { tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing error report")?; }
    assert_eq!(*seen.lock().map_err(|_| "poisoned")?, vec![0,1,2,3]); stop().await?; Ok(())
}

#[tokio::test]
async fn throwing_reporter_does_not_interrupt_other_subscribers_or_future_frames() -> Result<(), ServiceError> {
    let (reports, mut receive) = tokio::sync::mpsc::channel(4);
    let replica = ReplicatedStateReplica::new(Arc::new(move |error| { reports.try_send(error.to_string()).expect("report channel open"); panic!("reporter failed"); }));
    let stop_broken = replica.subscribe(Box::new(|_,_,_| Box::pin(async { Err("listener failed".into()) })));
    let seen = Arc::new(Mutex::new(Vec::new())); let output = seen.clone();
    let stop_good = replica.subscribe(Box::new(move |_,_,delivery| { output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(delivery.sequence.number() as u64); Box::pin(async { Ok(()) }) }));
    replica.hydrate(seq(0), &[Op::Replace(json!({"value":0}).into())], BACKGROUND_CONTEXT.clone())?;
    replica.update(seq(1), &[Op::Set(vec!["value".into()], json!(1).into())], BACKGROUND_CONTEXT.clone())?;
    for _ in 0..2 { tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing report")?; }
    assert_eq!(*seen.lock().map_err(|_| "poisoned")?, vec![0,1]);
    stop_broken().await?; stop_good().await?; Ok(())
}

#[test]
fn replica_contiguity_uses_javascript_increment_at_precision_boundary() -> Result<(), ServiceError> {
    let state = ReplicatedStateReplica::new(Arc::new(|error| panic!("{error}")));
    state.hydrate(seq(9_007_199_254_740_992), &[Op::Replace(json!({"value":0}).into())], BACKGROUND_CONTEXT.clone())?;
    state.update(seq(9_007_199_254_740_992), &[Op::Set(vec!["value".into()], json!(1).into())], BACKGROUND_CONTEXT.clone())?;
    assert_eq!(*state.value().ok_or("missing value")?, JsonValue::from(json!({"value":1})));
    Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
