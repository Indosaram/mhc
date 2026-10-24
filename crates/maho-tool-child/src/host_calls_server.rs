//! senpi `modes/rpc/tool-child/host-calls-server.ts` (pin 26acd9353).
//!
//! The server side of the reverse bridge (senpi#3566 todo 7). Each `host_call` from the child is admitted on receipt
//! against its job's capacity - the same one the child enforces - and run, queued FIFO, or refused with
//! `host_call_refused` naming its `call_id`; every admitted call is answered exactly once with `host_reply`. A job that
//! ends signals its running calls and answers its queued calls without running them.
//!
//! `ChannelSend` stands in for `Pick<ToolChildChannel, "send">`; the real `channel::ToolChildChannel` implements it.
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::future::{BoxFuture, FutureExt, Shared};
use maho_ext_api::AbortSignal;
use serde_json::Value;

use crate::admission::{HostCallAdmission, HostCallDecision};
pub use crate::channel::ChannelSend;
use crate::payload_ref::AbortSignal as RefAbortSignal;
use crate::payload_ref::{PayloadRefReader, PayloadRefWriter};
use crate::payload_wire::{PackedPayload, pack_payload, unpack_payload};
use crate::protocol::{
    ChildToServer, HostCallKind, PayloadRef, RefNackReason, ServerToChild, ToolChildMessage,
};

/// The wire name of a host-call kind (its `HOST_CALL_KINDS` entry), used in error labels.
pub fn host_call_kind_name(kind: HostCallKind) -> &'static str {
    match kind {
        HostCallKind::Tool => "tool",
        HostCallKind::Completion => "completion",
        HostCallKind::Reserved => "reserved",
        HostCallKind::ListTools => "list_tools",
        HostCallKind::Authorize => "authorize",
        HostCallKind::LockFile => "lock_file",
        HostCallKind::UnlockFile => "unlock_file",
    }
}

/// Bridges a job's `AbortSignal` to the flag `unpack_payload` polls between file chunks: the returned signal is set
/// when `signal` aborts. The watcher stops with the returned guard.
pub(crate) fn ref_signal(signal: &AbortSignal) -> (RefAbortSignal, tokio::task::JoinHandle<()>) {
    let bridged = RefAbortSignal::new();
    let (target, source) = (bridged.clone(), signal.clone());
    let watcher = tokio::spawn(async move {
        source.cancelled().await;
        target.abort();
    });
    (bridged, watcher)
}

/// The side's ref writer (payloads it sends) and reader (payloads it receives); absent = inline only.
#[derive(Clone)]
pub struct HostCallRefs {
    pub writer: Arc<PayloadRefWriter>,
    pub reader: Arc<PayloadRefReader>,
}

/// What the job's dispatcher attached to a call (the calling tool call, for a session); `None` when none.
pub type HostCallContext = Option<Arc<dyn Any + Send + Sync>>;

/// What a handler may know about the generation a call came from (file locks are held per generation).
#[derive(Clone)]
pub struct HostCallGeneration {
    /// Settles once the child that made the call has exited (observed by the server).
    pub exited: Shared<BoxFuture<'static, ()>>,
    /// Whether the calling job is still live (the server has not settled it).
    pub job_live: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Settles, with the holder's name, if the calling job becomes stuck: it outlived its deadline and grace and nothing
    /// can stop it (an in-process module). Its locks stay held, and every waiter on them fails fast. A child job never
    /// gets stuck - its process is killed - so this is `None` there.
    pub stuck: Option<Shared<BoxFuture<'static, String>>>,
}

pub struct HostCallRequest {
    pub call_id: String,
    pub job_id: String,
    pub kind: HostCallKind,
    pub name: Option<String>,
    pub args: Option<Value>,
    pub signal: AbortSignal,
    /// What the job's dispatcher attached to it (the calling tool call, for a session); `None` when none.
    pub context: HostCallContext,
    /// The child generation the call came from: whether its job is still live, and when the child's exit is observed.
    pub generation: HostCallGeneration,
}

/// The generation facts `ServerHostCalls` hands each request.
#[derive(Clone)]
pub struct HostCallScope {
    pub exited: Shared<BoxFuture<'static, ()>>,
    pub job_live: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl HostCallScope {
    /// `UNSCOPED`: an exit that never settles, every job live.
    pub fn unscoped() -> Self {
        Self {
            exited: futures::future::pending::<()>().boxed().shared(),
            job_live: Arc::new(|_| true),
        }
    }
}

/// A handler's outcome: its value, or the message the `host_reply` carries as `error`.
pub type HostCallHandler =
    Arc<dyn Fn(HostCallRequest) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

/// The job context a request carries; `ServerHostCalls` reads it per call.
pub type HostCallContextOf = Arc<dyn Fn(&str) -> HostCallContext + Send + Sync>;

#[derive(Clone)]
struct HostCallFrame {
    call_id: String,
    job_id: String,
    kind: HostCallKind,
    name: Option<String>,
    args: Option<Value>,
    args_ref: Option<PayloadRef>,
}

struct ServerCall {
    frame: HostCallFrame,
    abort: AbortSignal,
}

#[derive(Default)]
struct State {
    admissions: HashMap<String, HostCallAdmission>,
    calls: HashMap<String, ServerCall>,
}

impl State {
    fn admission(&mut self, job_id: &str) -> &mut HostCallAdmission {
        self.admissions.entry(job_id.to_owned()).or_default()
    }

    fn forget_idle(&mut self, job_id: &str) {
        if let Some(admission) = self.admissions.get(job_id)
            && admission.active() == 0
            && admission.waiting() == 0
        {
            self.admissions.remove(job_id);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Inner {
    channel: Arc<dyn ChannelSend>,
    handler: HostCallHandler,
    context_of: HostCallContextOf,
    refs: Option<HostCallRefs>,
    scope: HostCallScope,
    state: Mutex<State>,
}

pub struct ServerHostCalls {
    inner: Arc<Inner>,
}

impl ServerHostCalls {
    /// `context_of` defaults to no context and `scope` to `HostCallScope::unscoped()` when `None`.
    pub fn new(
        channel: Arc<dyn ChannelSend>,
        handler: HostCallHandler,
        context_of: Option<HostCallContextOf>,
        refs: Option<HostCallRefs>,
        scope: Option<HostCallScope>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                channel,
                handler,
                context_of: context_of.unwrap_or_else(|| Arc::new(|_| None)),
                refs,
                scope: scope.unwrap_or_else(HostCallScope::unscoped),
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// Calls running and queued for one job (0 for a job with none): `(active, waiting)`.
    pub fn load(&self, job_id: &str) -> (usize, usize) {
        let state = lock(&self.inner.state);
        match state.admissions.get(job_id) {
            Some(admission) => (admission.active(), admission.waiting()),
            None => (0, 0),
        }
    }

    /// Takes a `host_call`; returns false for every other frame. Must run inside a tokio runtime.
    pub fn handle(&self, frame: &ToolChildMessage) -> bool {
        let ToolChildMessage::Child(ChildToServer::HostCall {
            call_id,
            job_id,
            kind,
            name,
            args,
            args_ref,
        }) = frame
        else {
            return false;
        };
        let data = HostCallFrame {
            call_id: call_id.clone(),
            job_id: job_id.clone(),
            kind: *kind,
            name: name.clone(),
            args: args.clone(),
            args_ref: args_ref.clone(),
        };
        let (refusal, run_now) = {
            let mut state = lock(&self.inner.state);
            match state.admission(job_id).submit(call_id) {
                HostCallDecision::Refused { frame } => {
                    state.forget_idle(job_id);
                    (Some(*frame), false)
                }
                decision => {
                    state.calls.insert(
                        call_id.clone(),
                        ServerCall {
                            frame: data,
                            abort: AbortSignal::default(),
                        },
                    );
                    (None, decision == HostCallDecision::Active)
                }
            }
        };
        if let Some(frame) = refusal {
            self.inner.channel.send(frame);
            return true;
        }
        if run_now {
            run(&self.inner, call_id);
        }
        true
    }

    /// The job ended: its running calls are signalled, its queued calls are answered without running.
    pub fn end_job(&self, job_id: &str) {
        let aborts: Vec<AbortSignal> = lock(&self.inner.state)
            .calls
            .values()
            .filter(|call| call.frame.job_id == job_id)
            .map(|call| call.abort.clone())
            .collect();
        for abort in aborts {
            abort.abort();
        }
    }
}

fn run(inner: &Arc<Inner>, call_id: &str) {
    let Some((frame, abort)) = lock(&inner.state)
        .calls
        .get(call_id)
        .map(|call| (call.frame.clone(), call.abort.clone()))
    else {
        return;
    };
    let label = frame
        .name
        .clone()
        .unwrap_or_else(|| host_call_kind_name(frame.kind).to_owned());
    let invoke = {
        let inner = inner.clone();
        let frame = frame.clone();
        let abort = abort.clone();
        move |args: Option<Value>| -> BoxFuture<'static, Result<Value, String>> {
            let live = inner.scope.job_live.clone();
            let job_id = frame.job_id.clone();
            let request = HostCallRequest {
                call_id: frame.call_id.clone(),
                job_id: frame.job_id.clone(),
                kind: frame.kind,
                name: frame.name.clone(),
                args,
                signal: abort.clone(),
                context: (inner.context_of)(&frame.job_id),
                generation: HostCallGeneration {
                    exited: inner.scope.exited.clone(),
                    job_live: Arc::new(move || live(&job_id)),
                    stuck: None,
                },
            };
            (inner.handler)(request)
        }
    };
    let answered: BoxFuture<'static, Result<Value, String>> = if abort.is_aborted() {
        let inner = inner.clone();
        let frame = frame.clone();
        async move {
            refuse_args(&inner, &frame);
            Err(format!("{label}: the job ended before this call ran"))
        }
        .boxed()
    } else if frame.args_ref.is_none() {
        // Inline arguments start the handler in this turn, as admission promised; only a ref waits on its read.
        invoke(frame.args.clone())
    } else {
        let inner = inner.clone();
        let frame = frame.clone();
        async move {
            let args = read_args(&inner, &frame, &abort).await?;
            invoke(Some(args)).await
        }
        .boxed()
    };
    let inner = inner.clone();
    let call_id = call_id.to_owned();
    tokio::spawn(async move {
        match answered.await {
            Ok(value) => reply(&inner, &call_id, value).await,
            Err(error) => answer(
                &inner,
                &call_id,
                ToolChildMessage::Server(ServerToChild::HostReply {
                    call_id: call_id.clone(),
                    value: None,
                    value_ref: None,
                    error: Some(error),
                }),
            ),
        }
    });
}

/// The call's arguments: inline, or a child-written ref accepted (acked, now owned here) and consumed.
async fn read_args(
    inner: &Inner,
    frame: &HostCallFrame,
    signal: &AbortSignal,
) -> Result<Value, String> {
    let Some(args_ref) = &frame.args_ref else {
        return Ok(frame.args.clone().unwrap_or(Value::Null));
    };
    let Some(refs) = &inner.refs else {
        return Err("host_call carried argsRef but this side reads no refs".to_owned());
    };
    let (bridged, watcher) = ref_signal(signal);
    let outcome = unpack_payload(args_ref, &refs.reader, Some(&bridged))
        .await
        .map_err(|error| error.to_string());
    watcher.abort();
    outcome
}

/// A call whose job ended before it ran never opens its ref: the child's file is nacked and deleted by its writer.
fn refuse_args(inner: &Inner, frame: &HostCallFrame) {
    if let (Some(args_ref), Some(refs)) = (&frame.args_ref, &inner.refs) {
        refs.reader.refuse(args_ref, RefNackReason::Cancelled);
    }
}

/// A reply above the inline threshold travels as a server-written ref the child acks and then owns.
async fn reply(inner: &Arc<Inner>, call_id: &str, value: Value) {
    let packed = match &inner.refs {
        None => Ok(PackedPayload::Inline(value)),
        Some(refs) => pack_payload(&value, &refs.writer).await,
    };
    let message = match packed {
        Ok(PackedPayload::Inline(value)) => ToolChildMessage::Server(ServerToChild::HostReply {
            call_id: call_id.to_owned(),
            value: Some(value),
            value_ref: None,
            error: None,
        }),
        Ok(PackedPayload::Ref(payload_ref)) => ToolChildMessage::Server(ServerToChild::HostReply {
            call_id: call_id.to_owned(),
            value: None,
            value_ref: Some(payload_ref),
            error: None,
        }),
        Err(error) => ToolChildMessage::Server(ServerToChild::HostReply {
            call_id: call_id.to_owned(),
            value: None,
            value_ref: None,
            error: Some(error.to_string()),
        }),
    };
    answer(inner, call_id, message);
}

fn answer(inner: &Arc<Inner>, call_id: &str, reply: ToolChildMessage) {
    let promoted = {
        let mut state = lock(&inner.state);
        let Some(call) = state.calls.remove(call_id) else {
            return;
        };
        // Sent under the state lock so replies leave in the order calls settle.
        inner.channel.send(reply);
        let job_id = call.frame.job_id;
        let promoted = state
            .admissions
            .get_mut(&job_id)
            .and_then(|admission| admission.settle(call_id));
        state.forget_idle(&job_id);
        promoted
    };
    if let Some(promoted) = promoted {
        run(inner, &promoted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    // senpi#3566 todo 7: the server's own host-call admission, driven the way a misbehaving child would - 265 calls
    // sent at once without the child's admission in front of them.

    #[derive(Default)]
    struct RecordingChannel {
        sent: Mutex<Vec<ToolChildMessage>>,
    }

    impl ChannelSend for RecordingChannel {
        fn send(&self, message: ToolChildMessage) -> bool {
            lock(&self.sent).push(message);
            true
        }
    }

    impl RecordingChannel {
        fn frames(&self) -> Vec<ToolChildMessage> {
            lock(&self.sent).clone()
        }
    }

    fn host_call(call_id: &str, job_id: &str, args: Option<Value>) -> ToolChildMessage {
        ToolChildMessage::Child(ChildToServer::HostCall {
            call_id: call_id.to_owned(),
            job_id: job_id.to_owned(),
            kind: HostCallKind::Tool,
            name: Some("count".to_owned()),
            args,
            args_ref: None,
        })
    }

    /// One loop turn: enough for an already-woken task to run, never a timed wait.
    async fn turn() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    type Held = Arc<Mutex<Vec<oneshot::Sender<()>>>>;

    fn holding_handler(held: Held, ran: Arc<Mutex<usize>>) -> HostCallHandler {
        Arc::new(move |_request| {
            *lock(&ran) += 1;
            let (release, released) = oneshot::channel();
            lock(&held).push(release);
            async move {
                let _ = released.await;
                Ok(Value::String("ok".to_owned()))
            }
            .boxed()
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runs_8_queues_256_refuses_the_265th_by_its_call_id_then_answers_all_264_in_order() {
        let channel = Arc::new(RecordingChannel::default());
        let held: Held = Arc::default();
        let ran = Arc::new(Mutex::new(0));
        let calls = ServerHostCalls::new(
            channel.clone(),
            holding_handler(held.clone(), ran.clone()),
            None,
            None,
            None,
        );
        for index in 1..=265 {
            calls.handle(&host_call(
                &format!("c{index}"),
                "j1",
                Some(serde_json::json!({ "index": index })),
            ));
        }
        assert_eq!((calls.load("j1"), lock(&held).len()), ((8, 256), 8));
        assert_eq!(
            channel.frames(),
            vec![ToolChildMessage::Server(ServerToChild::HostCallRefused {
                call_id: "c265".to_owned(),
                code: "host_call_capacity".to_owned()
            })]
        );
        while !lock(&held).is_empty() {
            let releases: Vec<_> = lock(&held).drain(..).collect();
            for release in releases {
                let _ = release.send(());
            }
            turn().await;
        }
        let replies: Vec<String> = channel
            .frames()
            .into_iter()
            .filter_map(|frame| match frame {
                ToolChildMessage::Server(ServerToChild::HostReply { call_id, .. }) => Some(call_id),
                _ => None,
            })
            .collect();
        let expected: Vec<String> = (1..=264).map(|index| format!("c{index}")).collect();
        assert_eq!(replies, expected);
        assert_eq!(calls.load("j1"), (0, 0));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn answers_a_queued_call_of_an_ended_job_with_an_error_never_running_it() {
        let channel = Arc::new(RecordingChannel::default());
        let held: Held = Arc::default();
        let ran = Arc::new(Mutex::new(0));
        let calls = ServerHostCalls::new(
            channel.clone(),
            holding_handler(held.clone(), ran.clone()),
            None,
            None,
            None,
        );
        for index in 1..=9 {
            calls.handle(&host_call(&format!("c{index}"), "j1", Some(Value::Null)));
        }
        calls.end_job("j1");
        let releases: Vec<_> = lock(&held).drain(..).collect();
        for release in releases {
            let _ = release.send(());
        }
        turn().await;
        turn().await;
        assert_eq!(*lock(&ran), 8);
        let c9 = channel.frames().into_iter().find_map(|frame| match frame {
            ToolChildMessage::Server(ServerToChild::HostReply { call_id, error, .. })
                if call_id == "c9" =>
            {
                Some(error)
            }
            _ => None,
        });
        let Some(Some(error)) = c9 else {
            panic!("c9 was not answered with an error");
        };
        assert!(error.contains("job ended"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn holds_each_job_to_its_own_capacity_a_full_job_never_delays_another_jobs_call() {
        let channel = Arc::new(RecordingChannel::default());
        let running: Arc<Mutex<Vec<String>>> = Arc::default();
        let handler: HostCallHandler = {
            let running = running.clone();
            Arc::new(move |request| {
                lock(&running).push(request.call_id);
                futures::future::pending::<Result<Value, String>>().boxed()
            })
        };
        let calls = ServerHostCalls::new(channel.clone(), handler, None, None, None);
        for index in 1..=264 {
            calls.handle(&host_call(&format!("a{index}"), "j1", Some(Value::Null)));
        }
        calls.handle(&host_call("b1", "j2", Some(Value::Null)));
        assert_eq!(calls.load("j1"), (8, 256));
        assert_eq!(calls.load("j2"), (1, 0));
        assert!(lock(&running).contains(&"b1".to_owned()));
        assert!(channel.frames().is_empty());
    }
}
