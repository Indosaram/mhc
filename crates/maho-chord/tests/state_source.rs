use maho_chord::{context::BACKGROUND_CONTEXT, delta::Op, services::state::attach_replicated_state_source, types::*};
use serde_json::json;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};

type Receiver = Box<dyn FnMut(ReplicatedStateSourceFrame<JsonValue>) + Send>;
struct Source { receive: Arc<Mutex<Option<Receiver>>>, disposed: Arc<AtomicBool> }
struct Attachment { snapshot: ReplicatedStateSourceSnapshot<JsonValue>, receive: Arc<Mutex<Option<Receiver>>>, disposed: Arc<AtomicBool> }
impl ReplicatedStateSourceAttachment<JsonValue> for Attachment {
    fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<JsonValue> { &self.snapshot }
    fn activate(&mut self, listener: Receiver) -> Result<(), ServiceError> { *self.receive.lock().map_err(|_| "poisoned")? = Some(listener); Ok(()) }
    fn dispose(&mut self) { self.disposed.store(true, Ordering::Release); }
}
#[test]
fn source_snapshot_accepts_negative_safe_integer_cursors() -> Result<(), ServiceError> {
    struct Negative;
    impl ReplicatedStateSource<JsonValue> for Negative {
        fn attach(&self) -> Result<Box<dyn ReplicatedStateSourceAttachment<JsonValue>>, ServiceError> {
            Ok(Box::new(Attachment { snapshot: ReplicatedStateSourceSnapshot { cursor: -2, value: Arc::new(json!({"value":0}).into()) }, receive: Arc::new(Mutex::new(None)), disposed: Arc::new(AtomicBool::new(false)) }))
        }
    }
    let state = attach_replicated_state_source(&Negative, ReplicatedStateSourceOptions { on_error: None })?;
    assert_eq!(*state.value().ok_or("missing value")?, JsonValue::from(json!({"value":0})));
    state.dispose(); Ok(())
}
impl ReplicatedStateSource<JsonValue> for Source {
    fn attach(&self) -> Result<Box<dyn ReplicatedStateSourceAttachment<JsonValue>>, ServiceError> {
        Ok(Box::new(Attachment { snapshot: ReplicatedStateSourceSnapshot { cursor: 7, value: Arc::new(json!({"value":0}).into()) }, receive: self.receive.clone(), disposed: self.disposed.clone() }))
    }
}
#[test]
fn source_cursor_is_independent_of_local_sequence_and_gaps_dispose() -> Result<(), ServiceError> {
    let source = Source { receive: Arc::new(Mutex::new(None)), disposed: Arc::new(AtomicBool::new(false)) };
    let errors = Arc::new(Mutex::new(Vec::new())); let output = errors.clone();
    let state = attach_replicated_state_source(&source, ReplicatedStateSourceOptions { on_error: Some(Arc::new(move |error| output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(error.to_string()))) })?;
    let values = Arc::new(Mutex::new(Vec::new())); let output = values.clone();
    let stop = state.subscribe(Box::new(move |_, _, delivery| { output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(delivery.sequence.number() as u64); Box::pin(async { Ok(()) }) }));
    let mut receiver = source.receive.lock().map_err(|_| "poisoned")?.take().ok_or("missing source listener")?;
    receiver(ReplicatedStateSourceFrame { cursor: 8, value: Arc::new(json!({"value":1}).into()), ops: Arc::from([Op::Set(vec!["value".into()], json!(1).into())]), context: BACKGROUND_CONTEXT.clone() });
    assert_eq!(*values.lock().map_err(|_| "poisoned")?, vec![0,1]);
    receiver(ReplicatedStateSourceFrame { cursor: 10, value: Arc::new(json!({"value":2}).into()), ops: Arc::from([]), context: BACKGROUND_CONTEXT.clone() });
    assert!(source.disposed.load(Ordering::Acquire));
    assert_eq!(errors.lock().map_err(|_| "poisoned")?.len(), 1);
    assert_eq!(*state.value().ok_or("missing value")?, JsonValue::from(json!({"value":1})));
    drop(stop);
    Ok(())
}
#[test]
fn authoritative_source_values_and_operations_are_not_replayed_or_rediffed() -> Result<(), ServiceError> {
    let source = Source { receive: Arc::new(Mutex::new(None)), disposed: Arc::new(AtomicBool::new(false)) };
    let state = attach_replicated_state_source(&source, ReplicatedStateSourceOptions { on_error: Some(Arc::new(|error| panic!("{error}"))) })?;
    let value = Arc::new(JsonValue::from(json!({"value":999})));
    let delivered = Arc::new(Mutex::new(None)); let output = delivered.clone();
    let stop = state.subscribe(Box::new(move |value, _, delivery| { if delivery.sequence.number() as u64 == 1 { *output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(value); } Box::pin(async { Ok(()) }) }));
    let mut receiver = source.receive.lock().map_err(|_| "poisoned")?.take().ok_or("missing receiver")?;
    receiver(ReplicatedStateSourceFrame { cursor: 8, value: value.clone(), ops: Arc::from([Op::Set(vec!["value".into()], json!(1).into())]), context: BACKGROUND_CONTEXT.clone() });
    assert!(Arc::ptr_eq(&value, &state.value().ok_or("missing value")?));
    assert!(Arc::ptr_eq(&value, &delivered.lock().map_err(|_| "poisoned")?.clone().ok_or("missing delivered value")?));
    state.dispose(); state.dispose();
    receiver(ReplicatedStateSourceFrame { cursor: 9, value: Arc::new(json!({"value":2}).into()), ops: Arc::from([]), context: BACKGROUND_CONTEXT.clone() });
    assert!(Arc::ptr_eq(&value, &state.value().ok_or("missing retained value")?));
    drop(stop); Ok(())
}

struct StartupSource { failure: bool, disposal_panics: bool, disposals: Arc<std::sync::atomic::AtomicUsize> }
struct StartupAttachment { snapshot: ReplicatedStateSourceSnapshot<JsonValue>, failure: bool, disposal_panics: bool, disposals: Arc<std::sync::atomic::AtomicUsize> }
impl ReplicatedStateSource<JsonValue> for StartupSource {
    fn attach(&self) -> Result<Box<dyn ReplicatedStateSourceAttachment<JsonValue>>, ServiceError> {
        Ok(Box::new(StartupAttachment { snapshot: ReplicatedStateSourceSnapshot { value: Arc::new(json!({"value":0}).into()), cursor: 0 }, failure: self.failure, disposal_panics: self.disposal_panics, disposals: self.disposals.clone() }))
    }
}
impl ReplicatedStateSourceAttachment<JsonValue> for StartupAttachment {
    fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<JsonValue> { &self.snapshot }
    fn activate(&mut self, mut listener: Receiver) -> Result<(), ServiceError> {
        listener(ReplicatedStateSourceFrame { cursor: if self.failure { 2 } else { 1 }, value: Arc::new(json!({"value":1}).into()), ops: Arc::from([]), context: BACKGROUND_CONTEXT.clone() });
        listener(ReplicatedStateSourceFrame { cursor: 2, value: Arc::new(json!({"value":2}).into()), ops: Arc::from([]), context: BACKGROUND_CONTEXT.clone() });
        Ok(())
    }
    fn dispose(&mut self) { self.disposals.fetch_add(1, Ordering::AcqRel); if self.disposal_panics { panic!("dispose failed"); } }
}

#[test]
fn activation_frames_are_applied_before_attach_returns() -> Result<(), ServiceError> {
    let source = StartupSource { failure: false, disposal_panics: false, disposals: Arc::new(std::sync::atomic::AtomicUsize::new(0)) };
    let state = attach_replicated_state_source(&source, ReplicatedStateSourceOptions { on_error: Some(Arc::new(|error| panic!("{error}"))) })?;
    assert_eq!(*state.value().ok_or("missing value")?, JsonValue::from(json!({"value":2})));
    let sequences = Arc::new(Mutex::new(Vec::new())); let output = sequences.clone();
    let stop = state.subscribe(Box::new(move |_,_,delivery| { output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(delivery.sequence.number() as u64); Box::pin(async { Ok(()) }) }));
    assert_eq!(*sequences.lock().map_err(|_| "poisoned")?, vec![2]);
    state.dispose(); state.dispose();
    assert_eq!(source.disposals.load(Ordering::Acquire), 1);
    drop(stop); Ok(())
}

#[test]
fn invalid_activation_frame_stops_following_frames_and_disposes_once() -> Result<(), ServiceError> {
    let source = StartupSource { failure: true, disposal_panics: false, disposals: Arc::new(std::sync::atomic::AtomicUsize::new(0)) };
    let reports = Arc::new(Mutex::new(Vec::new())); let output = reports.clone();
    let state = attach_replicated_state_source(&source, ReplicatedStateSourceOptions { on_error: Some(Arc::new(move |error| output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(error.to_string()))) })?;
    assert_eq!(*state.value().ok_or("missing value")?, JsonValue::from(json!({"value":0})));
    assert_eq!(reports.lock().map_err(|_| "poisoned")?.len(), 1);
    assert_eq!(source.disposals.load(Ordering::Acquire), 1);
    state.dispose();
    assert_eq!(source.disposals.load(Ordering::Acquire), 1);
    Ok(())
}

#[test]
fn source_error_reporter_can_dispose_and_read_without_lock_reentrancy() -> Result<(), ServiceError> {
    let source = Source { receive: Arc::new(Mutex::new(None)), disposed: Arc::new(AtomicBool::new(false)) };
    let target = Arc::new(Mutex::new(None::<Arc<maho_chord::services::state::AttachedReplicatedStateImpl<JsonValue>>>)); let owner = target.clone();
    let state = attach_replicated_state_source(&source, ReplicatedStateSourceOptions { on_error: Some(Arc::new(move |_| {
        let state = owner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().expect("state installed");
        state.dispose(); assert!(state.value().is_some());
    })) })?;
    *target.lock().map_err(|_| "poisoned")? = Some(state.clone());
    let mut receive = source.receive.lock().map_err(|_| "poisoned")?.take().ok_or("missing receiver")?;
    receive(ReplicatedStateSourceFrame { cursor: 9, value: Arc::new(json!({"value":1}).into()), ops: Arc::from([]), context: BACKGROUND_CONTEXT.clone() });
    assert!(source.disposed.load(Ordering::Acquire));
    *target.lock().map_err(|_| "poisoned")? = None;
    Ok(())
}

#[test]
fn disposal_panic_after_activation_contract_failure_is_reported() -> Result<(), ServiceError> {
    let source = StartupSource { failure: true, disposal_panics: true, disposals: Arc::new(std::sync::atomic::AtomicUsize::new(0)) };
    let reports = Arc::new(Mutex::new(Vec::new())); let output = reports.clone();
    let state = attach_replicated_state_source(&source, ReplicatedStateSourceOptions { on_error: Some(Arc::new(move |error| output.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(error.to_string()))) })?;
    assert_eq!(source.disposals.load(Ordering::Acquire), 1);
    assert_eq!(reports.lock().map_err(|_| "poisoned")?.len(), 2);
    assert_eq!(*state.value().ok_or("missing value")?, JsonValue::from(json!({"value":0})));
    Ok(())
}
