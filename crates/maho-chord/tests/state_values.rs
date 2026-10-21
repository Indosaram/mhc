use maho_chord::{
    context::BACKGROUND_CONTEXT,
    delta::{resolve_value, Op},
    services::{state::*, state_internals::*},
    types::*,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn replacement_publishes_incremental_diff_operations() -> Result<(), ServiceError> {
    let state = MutableReplicatedStateImpl::new(json!({"value":0,"retained":{"large":"unchanged"}}).into())?;
    let ops = Arc::new(Mutex::new(Vec::new())); let output = ops.clone();
    let stop = state.subscribe_source(Arc::new(move |operations, _, _| { output.lock().map_err(|_| "poisoned")?.extend_from_slice(operations); Ok(()) }));
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1,"retained":{"large":"unchanged"}}).into())?;
    assert_eq!(*ops.lock().map_err(|_| "poisoned")?, vec![Op::Set(vec!["value".into()], json!(1).into())]);
    stop(); Ok(())
}

#[test]
fn restored_transactions_and_equal_replacements_preserve_revision_identity(
) -> Result<(), ServiceError> {
    let state = MutableReplicatedStateImpl::new(json!({"value":1}).into())?;
    let previous = state.value().ok_or("missing value")?;
    state.change(BACKGROUND_CONTEXT.clone(), &mut |change| {
        let draft = change.state()?;
        draft.set("value", &json!(2).into())?;
        draft.set("value", &json!(1).into())?;
        Ok(())
    })?;
    state.replace(BACKGROUND_CONTEXT.clone(), json!({"value":1}).into())?;
    assert!(Arc::ptr_eq(
        &state.value().ok_or("missing value")?,
        &previous
    ));
    assert_eq!(state.snapshot().1, ServiceSequence::ZERO);
    Ok(())
}
#[test]
fn assigned_json_is_copy_on_write_and_source_failures_follow_commit() -> Result<(), ServiceError> {
    let external: JsonValue = json!({"value":1}).into();
    let state = MutableReplicatedStateImpl::new(json!({"left":null,"right":null}).into())?;
    state.change(BACKGROUND_CONTEXT.clone(), &mut |change| {
        let draft = change.state()?;
        draft.set("left", &external)?;
        draft.set("right", &external)?;
        draft
            .get("left")?
            .ok_or("missing left")?
            .set("value", &json!(2).into())?;
        Ok(())
    })?;
    assert_eq!(external, JsonValue::from(json!({"value":1})));
    assert_eq!(
        *state.value().ok_or("missing state")?,
        JsonValue::from(json!({"left":{"value":2},"right":{"value":1}}))
    );
    let values = Arc::new(Mutex::new(Vec::new()));
    let output = values.clone();
    let broken = state.subscribe_source(Arc::new(|_, _, _| Err("failed source listener".into())));
    let good = state.subscribe_source(Arc::new(move |_, sequence, _| {
        output.lock().map_err(|_| "poisoned")?.push(sequence.number() as u64);
        Ok(())
    }));
    assert!(state
        .replace(BACKGROUND_CONTEXT.clone(), json!({"value":4}).into())
        .is_err());
    assert_eq!(
        *state.value().ok_or("missing value")?,
        JsonValue::from(json!({"value":4}))
    );
    assert_eq!(*values.lock().map_err(|_| "poisoned")?, vec![2]);
    broken();
    good();
    Ok(())
}
#[test]
fn replica_splices_share_unchanged_payloads_and_invalid_permutations_clear(
) -> Result<(), ServiceError> {
    let initial: JsonValue = json!({"rows":[{"value":1}]}).into();
    let inserted: JsonValue = json!({"value":2}).into();
    let replica = ReplicatedStateReplica::new(Arc::new(|error| panic!("{error}")));
    replica.hydrate(seq(0),
        &[Op::Replace(initial.clone())],
        BACKGROUND_CONTEXT.clone(),
    )?;
    replica.update(seq(1),
        &[Op::Splice(
            vec!["rows".into()],
            1,
            0,
            vec![inserted.clone()],
        )],
        BACKGROUND_CONTEXT.clone(),
    )?;
    let current = replica.value().ok_or("missing revision")?;
    assert!(resolve_value(&current, &["rows".into(), 0usize.into()])?
        .same_identity(resolve_value(&initial, &["rows".into(), 0usize.into()])?));
    assert!(resolve_value(&current, &["rows".into(), 1usize.into()])?.same_identity(&inserted));
    assert!(replica
        .update(seq(2),
            &[Op::Permute(vec!["rows".into()], vec![0])],
            BACKGROUND_CONTEXT.clone()
        )
        .is_err());
    assert!(replica.value().is_none());
    Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
