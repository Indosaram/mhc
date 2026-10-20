use maho_chord::{context::BACKGROUND_CONTEXT, delta::*, services::{state::*, state_internals::*}, types::*};
use std::result::Result;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn transaction_shares_retained_revisions_and_aborts_failed_drafts() -> Result<(), ServiceError> {
    let state = MutableReplicatedStateImpl::new(json!({"changed":{"value":1},"retained":{"value":2}}).into())?;
    let before = state.value().ok_or("missing value")?;
    let sequences = Arc::new(Mutex::new(Vec::new()));
    let received = sequences.clone();
    let stop = state.subscribe(Box::new(move |_, _, delivery| { received.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(delivery.sequence.number() as u64); Box::pin(async { Ok(()) }) }));
    state.change(BACKGROUND_CONTEXT.clone(), &mut |change| { change.state()?.get(&"changed".into())?.ok_or("missing child")?.set("value", &json!(4).into())?; Ok(()) })?;
    let after = state.value().ok_or("missing value")?;
    assert_eq!(*after, JsonValue::from(json!({"changed":{"value":4},"retained":{"value":2}})));
    assert!(resolve_value(&before, &["retained".into()])?.same_identity(resolve_value(&after, &["retained".into()])?));
    assert_eq!(*sequences.lock().map_err(|_| "poisoned")?, vec![0, 1]);
    let mut escaped = None;
    assert!(state.change(BACKGROUND_CONTEXT.clone(), &mut |change| { let draft = change.state()?; draft.set("new", &json!(true).into())?; escaped = Some(draft); Err("stop".into()) }).is_err());
    assert!(escaped.ok_or("missing escaped draft")?.value().is_err());
    assert!(Arc::ptr_eq(&state.value().ok_or("missing value")?, &after));
    drop(stop);
    Ok(())
}

#[test]
fn exact_publication_reentrancy_and_late_hydration_are_ordered() -> Result<(), ServiceError> {
    let state = MutableReplicatedStateImpl::new(json!({"value":0}).into())?;
    let exact = Arc::new(Mutex::new(Vec::new())); let output = exact.clone();
    let target = Arc::downgrade(&state);
    let remove = state.subscribe_source(Arc::new(move |_, sequence, _| {
        output.lock().map_err(|_| "poisoned")?.push(sequence.number() as u64);
        if sequence.number() == 1.0 { target.upgrade().ok_or("missing state")?.replace(BACKGROUND_CONTEXT.clone(), json!({"value":2}).into())?; }
        Ok(())
    }));
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into())?;
    assert_eq!(*exact.lock().map_err(|_| "poisoned")?, vec![1,2]);
    assert_eq!(state.snapshot().1, seq(2));
    remove();
    assert!(state.change(BACKGROUND_CONTEXT.clone(), &mut |_| state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":3}).into())).is_err());
    assert_eq!(*state.value().ok_or("missing value")?, JsonValue::from(json!({"value":2})));
    Ok(())
}

#[test]
fn replica_clears_on_sequence_gaps_and_rehydrates() -> Result<(), ServiceError> {
    let replica = ReplicatedStateReplica::new(Arc::new(|error| panic!("{error}")));
    assert!(replica.value().is_none());
    replica.hydrate(seq(4), &[Op::Replace(json!({"value":4}).into())], BACKGROUND_CONTEXT.clone())?;
    replica.update(seq(5), &[Op::Set(vec!["value".into()], json!(5).into())], BACKGROUND_CONTEXT.clone())?;
    assert!(replica.update(seq(7), &[], BACKGROUND_CONTEXT.clone()).is_err());
    assert!(replica.value().is_none());
    replica.hydrate(seq(10), &[Op::Replace(json!({"value":10}).into())], BACKGROUND_CONTEXT.clone())?;
    assert_eq!(*replica.value().ok_or("missing value")?, JsonValue::from(json!({"value":10})));
    Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
