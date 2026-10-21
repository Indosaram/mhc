use maho_chord::{context::{BACKGROUND_CONTEXT, with_cancel}, delta::Op, services::state::*, types::*};
use serde_json::json;
use std::{sync::{Arc, Mutex}, time::Duration};

type SourceReceiver = Box<dyn FnMut(ReplicatedStateSourceFrame<JsonValue>) + Send>;
struct Source(Arc<Mutex<Option<SourceReceiver>>>);
struct Attachment { snapshot: ReplicatedStateSourceSnapshot<JsonValue>, receiver: Arc<Mutex<Option<SourceReceiver>>> }
impl ReplicatedStateSource<JsonValue> for Source {
    fn attach(&self) -> Result<Box<dyn ReplicatedStateSourceAttachment<JsonValue>>, ServiceError> {
        Ok(Box::new(Attachment { snapshot: ReplicatedStateSourceSnapshot { value: Arc::new(json!({"value":0}).into()), cursor: 0 }, receiver: self.0.clone() }))
    }
}
impl ReplicatedStateSourceAttachment<JsonValue> for Attachment {
    fn snapshot(&self) -> &ReplicatedStateSourceSnapshot<JsonValue> { &self.snapshot }
    fn activate(&mut self, receiver: SourceReceiver) -> Result<(), ServiceError> { *self.receiver.lock().map_err(|_| "poisoned")? = Some(receiver); Ok(()) }
    fn dispose(&mut self) { self.receiver.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(); }
}
enum State { Mutable(Arc<MutableReplicatedStateImpl>), Replica(Arc<ReplicatedStateReplica>), Attached(Arc<AttachedReplicatedStateImpl<JsonValue>>, Source) }
impl State {
    fn new(kind: u8) -> Result<Self, ServiceError> {
        if kind == 2 { let source = Source(Arc::new(Mutex::new(None))); let state = attach_replicated_state_source(&source, ReplicatedStateSourceOptions { on_error: Some(Arc::new(|error| panic!("{error}"))) })?; Ok(Self::Attached(state, source)) }
        else if kind == 1 { let state = ReplicatedStateReplica::new(Arc::new(|e| panic!("{e}"))); state.hydrate(seq(0), &[Op::Replace(json!({"value":0}).into())], BACKGROUND_CONTEXT.clone())?; Ok(Self::Replica(state)) }
        else { Ok(Self::Mutable(MutableReplicatedStateImpl::new(json!({"value":0}).into())?)) }
    }
    fn subscribe(&self, listener: StateListener<JsonValue>) -> Disposal { match self { Self::Mutable(s) => s.subscribe(listener), Self::Replica(s) => s.subscribe(listener), Self::Attached(s, _) => s.subscribe(listener) } }
    fn publish(&self, sequence: u64, context: ContextRef) -> Result<(), ServiceError> { match self {
        Self::Mutable(s) => s.replace(context, json!({"value":sequence}).into()),
        Self::Replica(s) => s.update(seq(sequence), &[Op::Set(vec!["value".into()], json!(sequence).into())], context),
        Self::Attached(_, source) => {
            let mut receiver = source.0.lock().map_err(|_| "poisoned")?.take().ok_or("missing receiver")?;
            receiver(ReplicatedStateSourceFrame { cursor: i64::try_from(sequence)?, value: Arc::new(json!({"value":sequence}).into()), ops: Arc::from([Op::Set(vec!["value".into()], json!(sequence).into())]), context });
            *source.0.lock().map_err(|_| "poisoned")? = Some(receiver); Ok(())
        }
    } }
}
#[tokio::test]
async fn overflow_boundaries_exclude_running_callbacks() -> Result<(), ServiceError> {
    for replica in [0,1,2] { for count in [100_u64,101,102,201,202] {
        let state = State::new(replica)?;
        let (release, gate) = tokio::sync::oneshot::channel::<()>(); let gate = Arc::new(Mutex::new(Some(gate)));
        let (events, mut receive) = tokio::sync::mpsc::channel(110);
        let stop = state.subscribe(Box::new(move |_, _, delivery| {
            let gate = gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(); let events = events.clone();
            Box::pin(async move { if let Some(gate) = gate { gate.await?; } events.send(delivery.sequence.number() as u64).await.map_err(|e| e.to_string())?; Ok(()) })
        }));
        for sequence in 1..=count { state.publish(sequence, BACKGROUND_CONTEXT.clone())?; }
        release.send(()).map_err(|_| "gate dropped")?;
        let first = ((count - 1) / 100) * 100 + 1;
        let expected = std::iter::once(0).chain(first..=count).collect::<Vec<_>>(); let mut received = Vec::new();
        for _ in 0..expected.len() { received.push(tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing event")?); }
        assert_eq!(received, expected); stop().await?;
    } } Ok(())
}
#[tokio::test]
async fn running_update_is_not_counted_and_delivery_retains_context() -> Result<(), ServiceError> {
    for replica in [0,1,2] {
        let state = State::new(replica)?; let (context, _) = with_cancel(&BACKGROUND_CONTEXT);
        let (release, gate) = tokio::sync::oneshot::channel::<()>(); let gate = Arc::new(Mutex::new(Some(gate)));
        let (events, mut receive) = tokio::sync::mpsc::channel(110);
        let stop = state.subscribe(Box::new(move |value, context, delivery| {
            let gate = if delivery.sequence.number() as u64 == 1 { gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() } else { None }; let events = events.clone();
            Box::pin(async move { if let Some(gate) = gate { gate.await?; } events.send((value, context, delivery)).await.map_err(|e| e.to_string())?; Ok(()) })
        }));
        for sequence in 1..102 { state.publish(sequence, BACKGROUND_CONTEXT.clone())?; }
        state.publish(102, context.clone())?; state.publish(103, BACKGROUND_CONTEXT.clone())?;
        release.send(()).map_err(|_| "gate dropped")?;
        let mut received = Vec::new();
        for _ in 0..4 { received.push(tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing delivery")?); }
        assert_eq!(received.iter().map(|(_,_,d)| d.sequence.number() as u64).collect::<Vec<_>>(), vec![0,1,102,103]);
        assert!(Arc::ptr_eq(&received[2].1, &context));
        assert_eq!(*received[2].0, JsonValue::from(json!({"value":102})));
        stop().await?;
    } Ok(())
}
#[tokio::test]
async fn independent_subscriptions_and_unsubscribe_do_not_cancel_running_callbacks() -> Result<(), ServiceError> {
    for replica in [0,1,2] {
        let state = State::new(replica)?;
        let (release_first, gate_first) = tokio::sync::oneshot::channel::<()>(); let gate_first = Arc::new(Mutex::new(Some(gate_first)));
        let (release_second, gate_second) = tokio::sync::oneshot::channel::<()>(); let gate_second = Arc::new(Mutex::new(Some(gate_second)));
        let (events, mut receive) = tokio::sync::mpsc::channel(8);
        let mut stops = Vec::new();
        for (index, gate) in [gate_first, gate_second].into_iter().enumerate() {
            let events = events.clone();
            stops.push(state.subscribe(Box::new(move |_,_,delivery| {
                let gate = gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take(); let events = events.clone();
                Box::pin(async move { if let Some(gate) = gate { gate.await?; } events.send((index, delivery.sequence.number() as u64)).await.map_err(|e| e.to_string())?; Ok(()) })
            })));
        }
        state.publish(1, BACKGROUND_CONTEXT.clone())?;
        stops.remove(0)().await?;
        release_first.send(()).map_err(|_| "first gate dropped")?; release_second.send(()).map_err(|_| "second gate dropped")?;
        let mut received = Vec::new(); for _ in 0..3 { received.push(tokio::time::timeout(Duration::from_secs(1), receive.recv()).await?.ok_or("missing delivery")?); }
        received.sort(); assert_eq!(received, vec![(0,0),(1,0),(1,1)]);
        stops.remove(0)().await?;
    } Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
