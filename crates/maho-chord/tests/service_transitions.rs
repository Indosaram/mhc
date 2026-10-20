use maho_chord::{context::{BACKGROUND_CONTEXT, with_cancel}, services::consumer::RemoteServiceBindingImpl, types::*};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

struct Request { service_id: JsString, mode: ServiceMode, reply: oneshot::Sender<Box<dyn ServiceSubscription>> }
struct Transport { requests: mpsc::Sender<Request> }
impl RemoteServiceTransport for Transport {
    fn invoke(&self, _: ServiceCall, _: ContextRef) -> BoxFuture<'_, Result<Option<JsonValue>, ServiceError>> { Box::pin(async { Ok(None) }) }
    fn subscribe(&self, service_id: JsString, mode: ServiceMode, _: Arc<dyn Fn(ServiceProviderUpdate, ContextRef) + Send + Sync>, _: ContextRef) -> BoxFuture<'_, Result<Box<dyn ServiceSubscription>, ServiceError>> {
        Box::pin(async move { let (reply, response) = oneshot::channel(); self.requests.send(Request { service_id, mode, reply }).await.map_err(|e| e.to_string())?; Ok(response.await?) })
    }
}
struct Subscription { snapshot: ServiceSubscriptionSnapshot, closed: Option<oneshot::Sender<()>> }
impl ServiceSubscription for Subscription {
    fn snapshot(&self) -> &ServiceSubscriptionSnapshot { &self.snapshot }
    fn activate(&mut self) -> Result<(), ServiceError> { Ok(()) }
    fn close(&mut self, _: Option<ContextRef>) -> BoxFuture<'_, Result<(), ServiceError>> {
        let result = self.closed.take().map_or(Ok(()), |sender| sender.send(()).map_err(|_| "close receiver dropped"));
        Box::pin(async move { result.map_err(ServiceError::from) })
    }
}
fn binding(requests: mpsc::Sender<Request>) -> Result<Arc<RemoteServiceBindingImpl>, ServiceError> {
    RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: vec!["test".into()], transport: Arc::new(Transport { requests }), bound: true, on_error: Some(Arc::new(|e| panic!("{e}"))), assert_access: None })
}
fn reply(request: Request) -> Result<oneshot::Receiver<()>, ServiceError> {
    let (closed, completion) = oneshot::channel();
    request.reply.send(Box::new(Subscription { snapshot: ServiceSubscriptionSnapshot { service_id: request.service_id, mode: request.mode, instances: vec![ServiceInstanceSnapshot { instance: None, members: vec![ServiceMemberSnapshot::Method { name: "call".into() }] }] }, closed: Some(closed) })).map_err(|_| "request dropped")?;
    Ok(completion)
}
#[tokio::test]
async fn rebind_closes_stale_inflight_subscription_and_readiness_waits_for_new_start() -> Result<(), ServiceError> {
    let (sender, mut requests) = mpsc::channel(4); let binding = binding(sender)?;
    let _facade = binding.use_facade(&"test".into())?;
    let first = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing first request")?;
    binding.rebind(false, BACKGROUND_CONTEXT.clone()).await?;
    let closed_first = reply(first)?;
    tokio::time::timeout(Duration::from_secs(1), closed_first).await??;
    let target = binding.clone();
    let rebind = tokio::spawn(async move { target.rebind(true, BACKGROUND_CONTEXT.clone()).await });
    let second = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing second request")?;
    let closed_second = reply(second)?;
    tokio::time::timeout(Duration::from_secs(1), rebind).await???;
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    binding.dispose(BACKGROUND_CONTEXT.clone()).await?;
    tokio::time::timeout(Duration::from_secs(1), closed_second).await??;
    Ok(())
}
#[tokio::test]
async fn cancelled_readiness_does_not_cancel_subscription_startup() -> Result<(), ServiceError> {
    let (sender, mut requests) = mpsc::channel(4); let binding = binding(sender)?;
    let _facade = binding.use_facade(&"test".into())?;
    let first = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing request")?;
    let (context, cancel) = with_cancel(&BACKGROUND_CONTEXT); cancel.cancel();
    assert!(binding.ready(context).await.is_err());
    let closed = reply(first)?;
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    binding.dispose(BACKGROUND_CONTEXT.clone()).await?;
    tokio::time::timeout(Duration::from_secs(1), closed).await??;
    Ok(())
}

#[tokio::test]
async fn overlapping_rebind_supersedes_inflight_start_without_waiting_for_it() -> Result<(), ServiceError> {
    // Given a live binding whose replacement subscription is unresolved.
    let (sender, mut requests) = mpsc::channel(4); let binding = binding(sender)?;
    let facade = binding.use_facade(&"test".into())?;
    let initial = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing initial request")?;
    let closed_initial = reply(initial)?;
    binding.ready(BACKGROUND_CONTEXT.clone()).await?;
    let owner = binding.clone();
    let first = tokio::spawn(async move { owner.rebind(true, BACKGROUND_CONTEXT.clone()).await });
    let stale = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing replacement request")?;
    // When a later unbind supersedes that start before the transport resolves.
    tokio::time::timeout(Duration::from_secs(1), binding.rebind(false, BACKGROUND_CONTEXT.clone())).await??;
    tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await??;
    // Then the old request is closed once it lands and cannot restore connectivity.
    let closed_stale = reply(stale)?;
    tokio::time::timeout(Duration::from_secs(1), first).await???;
    tokio::time::timeout(Duration::from_secs(1), closed_stale).await??;
    tokio::time::timeout(Duration::from_secs(1), closed_initial).await??;
    assert!(facade.member(&"call".into())?.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    binding.dispose(BACKGROUND_CONTEXT.clone()).await?;
    Ok(())
}

#[tokio::test]
async fn guarded_handles_can_be_acquired_before_activation_but_not_used() -> Result<(), ServiceError> {
    // Given a deferred host and retained method/state handles.
    use std::sync::atomic::{AtomicBool, Ordering};
    let (sender, mut requests) = mpsc::channel(4);
    let active = Arc::new(AtomicBool::new(false)); let flag = active.clone();
    let binding = RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: vec!["test".into()], transport: Arc::new(Transport { requests: sender }), bound: false, on_error: Some(Arc::new(|error| panic!("{error}"))), assert_access: Some(Arc::new(move || if flag.load(Ordering::Acquire) { Ok(()) } else { Err("host inactive".into()) })) })?;
    let facade = binding.use_facade(&"test".into())?;
    let method = facade.member(&"call".into())?;
    assert!(method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    let state = facade.member(&"state".into())?;
    assert!(state.value().is_err());
    assert!(state.subscribe(Box::new(|_,_,_| Box::pin(async { Ok(()) }))).is_err());
    // When the host binds while its public access guard remains closed.
    let owner = binding.clone(); let rebind = tokio::spawn(async move { owner.rebind(true, BACKGROUND_CONTEXT.clone()).await });
    let request = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing activation request")?;
    let (closed, completion) = oneshot::channel();
    request.reply.send(Box::new(Subscription { snapshot: ServiceSubscriptionSnapshot { service_id: "test".into(), mode: ServiceMode::Singleton, instances: vec![ServiceInstanceSnapshot { instance: None, members: vec![ServiceMemberSnapshot::Method { name: "call".into() }, ServiceMemberSnapshot::State { name: "state".into(), sequence: seq(0), ops: vec![maho_chord::delta::Op::Replace(JsonValue::object(vec![]))] }] }] }, closed: Some(closed) })).map_err(|_| "request dropped")?;
    tokio::time::timeout(Duration::from_secs(1), rebind).await???;
    active.store(true, Ordering::Release);
    // Then the same handles work after activation and reject access after disposal.
    assert!(Arc::ptr_eq(&method, &facade.member(&"call".into())?));
    assert!(state.value()?.is_some());
    method.invoke(vec![], BACKGROUND_CONTEXT.clone()).await?;
    binding.dispose(BACKGROUND_CONTEXT.clone()).await?;
    tokio::time::timeout(Duration::from_secs(1), completion).await??;
    assert!(state.value().is_err());
    Ok(())
}

struct ReentrantSubscription {
    snapshot: ServiceSubscriptionSnapshot,
    binding: std::sync::Weak<RemoteServiceBindingImpl>,
    completed: Option<oneshot::Sender<Result<(), String>>>,
    closed: Option<oneshot::Sender<()>>,
}
impl ServiceSubscription for ReentrantSubscription {
    fn snapshot(&self) -> &ServiceSubscriptionSnapshot { &self.snapshot }
    fn activate(&mut self) -> Result<(), ServiceError> {
        let binding = self.binding.upgrade().ok_or("missing binding")?;
        let completed = self.completed.take().ok_or("activation repeated")?;
        tokio::spawn(async move { let result = binding.dispose(BACKGROUND_CONTEXT.clone()).await.map_err(|error| error.to_string()); completed.send(result).expect("disposal receiver open"); });
        Ok(())
    }
    fn close(&mut self, _: Option<ContextRef>) -> BoxFuture<'_, Result<(), ServiceError>> {
        if let Some(closed) = self.closed.take() { closed.send(()).expect("close receiver open"); }
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn disposal_requested_by_startup_callback_settles_and_closes_subscription() -> Result<(), ServiceError> {
    let (sender, mut requests) = mpsc::channel(4); let binding = binding(sender)?;
    let facade = binding.use_facade(&"test".into())?;
    let request = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing startup")?;
    let (completed, completion) = oneshot::channel(); let (closed, closure) = oneshot::channel();
    request.reply.send(Box::new(ReentrantSubscription { snapshot: ServiceSubscriptionSnapshot { service_id: "test".into(), mode: ServiceMode::Singleton, instances: vec![ServiceInstanceSnapshot { instance: None, members: vec![ServiceMemberSnapshot::Method { name: "call".into() }] }] }, binding: Arc::downgrade(&binding), completed: Some(completed), closed: Some(closed) })).map_err(|_| "request dropped")?;
    tokio::time::timeout(Duration::from_secs(1), completion).await??.map_err(ServiceError::from)?;
    tokio::time::timeout(Duration::from_secs(1), closure).await??;
    assert!(facade.member(&"call".into())?.invoke(vec![], BACKGROUND_CONTEXT.clone()).await.is_err());
    assert!(binding.ready(BACKGROUND_CONTEXT.clone()).await.is_err());
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("typed failure {0}")]
struct TypedFailure(u8);
enum CloseBehavior { Error, PanicBeforeFuture, PanicInFuture, Gated(oneshot::Receiver<()>) }
struct FailingSubscription {
    snapshot: ServiceSubscriptionSnapshot,
    close: Option<CloseBehavior>,
    entered: Option<oneshot::Sender<bool>>,
    released: Option<oneshot::Sender<()>>,
    activation_failure: bool,
}
impl ServiceSubscription for FailingSubscription {
    fn snapshot(&self) -> &ServiceSubscriptionSnapshot { &self.snapshot }
    fn activate(&mut self) -> Result<(), ServiceError> { if self.activation_failure { Err(Box::new(TypedFailure(1))) } else { Ok(()) } }
    fn close(&mut self, context: Option<ContextRef>) -> BoxFuture<'_, Result<(), ServiceError>> {
        if let Some(entered) = self.entered.take() { entered.send(context.and_then(|context| context.abort_signal()).is_none()).expect("close observer open"); }
        let behavior = self.close.take().expect("closed once");
        if matches!(&behavior, CloseBehavior::PanicBeforeFuture) { std::panic::panic_any(Box::new(TypedFailure(2)) as ServiceError); }
        let released = self.released.take();
        Box::pin(async move {
            match behavior {
                CloseBehavior::Error => Err(Box::new(TypedFailure(2)) as ServiceError),
                CloseBehavior::PanicBeforeFuture => unreachable!("synchronous panic handled before future construction"),
                CloseBehavior::PanicInFuture => std::panic::panic_any(Box::new(TypedFailure(2)) as ServiceError),
                CloseBehavior::Gated(gate) => { gate.await?; if let Some(released) = released { released.send(()).expect("release observer open"); } Ok(()) },
            }
        })
    }
}
fn failing_reply(request: Request, close: CloseBehavior, activation_failure: bool, entered: Option<oneshot::Sender<bool>>, released: Option<oneshot::Sender<()>>) -> Result<(), ServiceError> {
    request.reply.send(Box::new(FailingSubscription { snapshot: ServiceSubscriptionSnapshot { service_id: request.service_id, mode: request.mode, instances: vec![ServiceInstanceSnapshot { instance: None, members: vec![ServiceMemberSnapshot::Method { name: "call".into() }] }] }, close: Some(close), entered, released, activation_failure })).map_err(|_| "request dropped".into())
}
fn typed_failures(error: &(dyn std::error::Error + 'static), output: &mut Vec<u8>) {
    if let Some(error) = error.downcast_ref::<TypedFailure>() { output.push(error.0); }
    else if let Some(errors) = error.downcast_ref::<maho_chord::services::errors::CollectedErrors>() { for error in &errors.errors { typed_failures(error.as_ref(), output); } }
    else if let Some(source) = error.source() { typed_failures(source, output); }
}
fn quiet_binding(sender: mpsc::Sender<Request>) -> Result<Arc<RemoteServiceBindingImpl>, ServiceError> {
    RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: vec!["test".into()], transport: Arc::new(Transport { requests: sender }), bound: true, on_error: Some(Arc::new(|_| {})), assert_access: None })
}

#[tokio::test]
async fn startup_preserves_primary_and_cleanup_errors_for_repeated_readiness() -> Result<(), ServiceError> {
    for behavior in [CloseBehavior::Error, CloseBehavior::PanicBeforeFuture, CloseBehavior::PanicInFuture] {
        let (sender, mut requests) = mpsc::channel(4); let binding = quiet_binding(sender)?;
        binding.use_facade(&"test".into())?;
        let request = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing request")?;
        failing_reply(request, behavior, true, None, None)?;
        for _ in 0..2 {
            let error = tokio::time::timeout(Duration::from_secs(1), binding.ready(BACKGROUND_CONTEXT.clone())).await?.expect_err("initialization must fail");
            let mut failures = Vec::new(); typed_failures(error.as_ref(), &mut failures); assert_eq!(failures, vec![1,2]);
        }
        let error = tokio::time::timeout(Duration::from_secs(1), binding.dispose(BACKGROUND_CONTEXT.clone())).await?.expect_err("disposal retains startup errors");
        let mut failures = Vec::new(); typed_failures(error.as_ref(), &mut failures); assert_eq!(failures, vec![1,2]);
    }
    Ok(())
}

#[tokio::test]
async fn rebind_close_panics_settle_without_hiding_replacement_failure() -> Result<(), ServiceError> {
    for behavior in [CloseBehavior::Error, CloseBehavior::PanicBeforeFuture, CloseBehavior::PanicInFuture] {
        let (sender, mut requests) = mpsc::channel(4); let binding = quiet_binding(sender)?;
        binding.use_facade(&"test".into())?;
        let initial = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing initial")?;
        failing_reply(initial, behavior, false, None, None)?;
        binding.ready(BACKGROUND_CONTEXT.clone()).await?;
        let owner = binding.clone(); let transition = tokio::spawn(async move { owner.rebind(true, BACKGROUND_CONTEXT.clone()).await });
        let replacement = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("replacement skipped after close failure")?;
        failing_reply(replacement, CloseBehavior::Error, true, None, None)?;
        let error = tokio::time::timeout(Duration::from_secs(1), transition).await??.expect_err("transition must fail");
        let mut failures = Vec::new(); typed_failures(error.as_ref(), &mut failures); assert_eq!(failures, vec![2,1,2]);
        assert!(tokio::time::timeout(Duration::from_secs(1), binding.dispose(BACKGROUND_CONTEXT.clone())).await?.is_err());
    }
    Ok(())
}

#[tokio::test]
async fn disposal_owns_close_after_waiter_is_dropped_and_repeated_call_joins() -> Result<(), ServiceError> {
    let (sender, mut requests) = mpsc::channel(4); let binding = quiet_binding(sender)?;
    binding.use_facade(&"test".into())?;
    let initial = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing initial")?;
    let (release, gate) = oneshot::channel(); let (entered, entry) = oneshot::channel(); let (released, completion) = oneshot::channel();
    failing_reply(initial, CloseBehavior::Gated(gate), false, Some(entered), Some(released))?;
    binding.ready(BACKGROUND_CONTEXT.clone()).await?;
    let (context, cancel) = with_cancel(&BACKGROUND_CONTEXT); cancel.cancel();
    drop(binding.dispose(context));
    assert!(tokio::time::timeout(Duration::from_secs(1), entry).await??);
    release.send(()).map_err(|_| "owned cleanup was cancelled")?;
    tokio::time::timeout(Duration::from_secs(1), completion).await??;
    tokio::time::timeout(Duration::from_secs(1), binding.dispose(BACKGROUND_CONTEXT.clone())).await??;
    Ok(())
}

#[tokio::test]
async fn disposal_normalizes_each_close_failure_and_retains_same_error_for_later_waiters() -> Result<(), ServiceError> {
    for behavior in [CloseBehavior::Error, CloseBehavior::PanicBeforeFuture, CloseBehavior::PanicInFuture] {
        let (sender, mut requests) = mpsc::channel(4); let binding = quiet_binding(sender)?;
        binding.use_facade(&"test".into())?;
        let initial = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing initial")?;
        failing_reply(initial, behavior, false, None, None)?;
        binding.ready(BACKGROUND_CONTEXT.clone()).await?;
        for _ in 0..2 {
            let error = tokio::time::timeout(Duration::from_secs(1), binding.dispose(BACKGROUND_CONTEXT.clone())).await?.expect_err("close must fail");
            let mut failures = Vec::new(); typed_failures(error.as_ref(), &mut failures); assert_eq!(failures, vec![2]);
        }
    }
    Ok(())
}

#[tokio::test]
async fn disposal_attempts_every_binding_and_aggregates_close_errors_and_panics() -> Result<(), ServiceError> {
    let (sender, mut requests) = mpsc::channel(4);
    let binding = RemoteServiceBindingImpl::new(RemoteServiceBindingOptions { services: vec!["left".into(), "right".into()], transport: Arc::new(Transport { requests: sender }), bound: true, on_error: Some(Arc::new(|_| {})), assert_access: None })?;
    binding.use_facade(&"left".into())?; binding.use_facade(&"right".into())?;
    let mut entries = Vec::new();
    for behavior in [CloseBehavior::Error, CloseBehavior::PanicInFuture] {
        let request = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing request")?;
        let (entered, entry) = oneshot::channel(); entries.push(entry);
        failing_reply(request, behavior, false, Some(entered), None)?;
    }
    binding.ready(BACKGROUND_CONTEXT.clone()).await?;
    let error = tokio::time::timeout(Duration::from_secs(1), binding.dispose(BACKGROUND_CONTEXT.clone())).await?.expect_err("both closes fail");
    for entry in entries { assert!(tokio::time::timeout(Duration::from_secs(1), entry).await??); }
    let mut failures = Vec::new(); typed_failures(error.as_ref(), &mut failures); assert_eq!(failures, vec![2,2]);
    Ok(())
}

#[tokio::test]
async fn disposal_joins_superseded_start_and_preserves_its_stale_close_failure() -> Result<(), ServiceError> {
    let (sender, mut requests) = mpsc::channel(4); let binding = quiet_binding(sender)?;
    binding.use_facade(&"test".into())?;
    let stale = tokio::time::timeout(Duration::from_secs(1), requests.recv()).await?.ok_or("missing request")?;
    binding.rebind(false, BACKGROUND_CONTEXT.clone()).await?;
    drop(binding.dispose(BACKGROUND_CONTEXT.clone()));
    let (entered, entry) = oneshot::channel();
    failing_reply(stale, CloseBehavior::PanicBeforeFuture, false, Some(entered), None)?;
    assert!(tokio::time::timeout(Duration::from_secs(1), entry).await??);
    let error = tokio::time::timeout(Duration::from_secs(1), binding.dispose(BACKGROUND_CONTEXT.clone())).await?.expect_err("stale close failure retained");
    let mut failures = Vec::new(); typed_failures(error.as_ref(), &mut failures); assert_eq!(failures, vec![2]);
    Ok(())
}

fn seq(value: u64) -> ServiceSequence { ServiceSequence::new(value as f64).expect("small finite integral fixture sequence") }
