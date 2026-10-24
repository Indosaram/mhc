//! senpi `modes/rpc/tool-child/entry.ts` (pin 26acd9353): the tool child's main loop (senpi#3566, plan todo 4). It
//! reaches its server only through the private channel, runs each `job` frame through the handler registered for its
//! kind, and exits when the server goes away (parent-watch), so a child can never outlive the session that owns it. It
//! loads no session, provider, extension or MCP scope: the server is the single writer of every session file.
//!
//! Rust shape: `ToolChildJobContext`'s callbacks are shared async closures (`Arc<dyn Fn .. -> BoxFuture>`), a handler
//! is `Arc<dyn Fn(Value, ToolChildJobContext) -> BoxFuture<Result<Option<Value>, HandlerError>>>`, `exit` is a callback
//! (upstream's `never` return becomes "the loop returns right after calling it"), and `ChannelSender` adapts the real
//! `ToolChildChannel` to the `ChannelSend` trait the host-call / update modules take.
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt};
use maho_ext_api::AbortSignal;
use serde_json::Value;

use crate::channel::{ToolChildChannel, connect_channel};
pub use crate::entry_error::ToolChildJobError;
use crate::flow_control::WakeAdmission;
use crate::host_calls_child::{ChildHostCalls, HostCallTarget};
use crate::host_calls_server::{ChannelSend, HostCallRefs};
use crate::job_updates::{JobUpdates, JobUpdatesOptions};
use crate::launch_env_names::{
    ProcessEnv, TOOL_CHILD_LEDGER_ENV, TOOL_CHILD_REENTRY_ENV, TOOL_CHILD_REFS_ROOT_ENV,
    process_env, remember_tool_child_reentry, scrub_process_env,
};
use crate::launch_gate::LaunchGate;
use crate::orphan_sweep::sweep_ledger;
use crate::payload_ref::{
    AbortSignal as RefAbortSignal, PayloadRefReader, PayloadRefReaderOptions, PayloadRefWriter,
    PayloadRefWriterOptions,
};
use crate::payload_wire::unpack_payload;
use crate::protocol::{ChildToServer, PayloadRef, RefControl, ServerToChild, ToolChildMessage};
use crate::test_controls::{TestSwitch, read_test_controls};

pub const TOOL_CHILD_GENERATION_ENV: &str = "SENPI_TOOL_CHILD_GENERATION";
/// 2: `hello` carries the child's `jobKinds` manifest.
pub const TOOL_CHILD_PROTOCOL_VERSION: u64 = 2;

/// What a handler may fail with: a `ToolChildJobError` keeps its `code`, anything else is `tool_child_job_failed`.
pub type HandlerError = Box<dyn Error + Send + Sync>;

/// `Pick<ToolChildChannel, "send">` over the real channel.
#[derive(Clone)]
pub struct ChannelSender(pub ToolChildChannel);

impl ChannelSend for ChannelSender {
    fn send(&self, message: ToolChildMessage) -> bool {
        self.0.send(&message).unwrap_or(false)
    }
}

/// `{ survivesCancel?: boolean }`.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostCallOptions {
    pub survives_cancel: bool,
}

pub type HostCallFn = Arc<
    dyn Fn(HostCallTarget, HostCallOptions) -> BoxFuture<'static, Result<Value, ToolChildJobError>>
        + Send
        + Sync,
>;
pub type UpdateFn =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<(), ToolChildJobError>> + Send + Sync>;
pub type WakeFn =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<(), ToolChildJobError>> + Send + Sync>;

#[derive(Clone)]
pub struct ToolChildJobContext {
    pub job_id: String,
    pub signal: AbortSignal,
    pub channel: ToolChildChannel,
    pub gate: Arc<LaunchGate>,
    /// A call to the server for this job (reverse bridge); settles with its reply, refusal or the job's cancel. A call
    /// made with `survives_cancel` is not settled by the cancel (a lock must be released after the job's writes stop,
    /// not when the cancel arrives); it still settles by its reply, its refusal or the channel closing.
    pub host_call: HostCallFn,
    /// Streams a partial result to the server in order; waits for credit when too many are unacknowledged.
    pub update: UpdateFn,
    /// This child's generation (the server's count of its spawns).
    pub generation: u64,
    /// Tells the server something happened on this child outside any job's lifetime (a background session exited). The
    /// server hands it to the job definition whose kind is `source`. Never dropped: past the pending budget it waits.
    pub wake: WakeFn,
}

pub type ToolChildJobHandler = Arc<
    dyn Fn(Value, ToolChildJobContext) -> BoxFuture<'static, Result<Option<Value>, HandlerError>>
        + Send
        + Sync,
>;

pub struct ToolChildEntryOptions {
    pub handlers: BTreeMap<String, ToolChildJobHandler>,
    /// `None` reads (and then scrubs) the process environment.
    pub env: Option<ProcessEnv>,
    /// `None` is `std::process::exit`.
    pub exit: Option<Arc<dyn Fn(i32) + Send + Sync>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Session {
    channel: ToolChildChannel,
    gate: Arc<LaunchGate>,
    host_calls: Arc<ChildHostCalls>,
    refs: Option<HostCallRefs>,
    handlers: BTreeMap<String, ToolChildJobHandler>,
    running: Mutex<HashMap<String, AbortSignal>>,
    streams: Mutex<HashMap<String, Arc<JobUpdates>>>,
    generation: u64,
    wake: WakeFn,
}

pub async fn run_tool_child(options: ToolChildEntryOptions) -> Result<(), HandlerError> {
    let scrub = options.env.is_none();
    let env: ProcessEnv = options.env.unwrap_or_else(process_env);
    let exit: Arc<dyn Fn(i32) + Send + Sync> = options
        .exit
        .unwrap_or_else(|| Arc::new(|code| std::process::exit(code)));
    let generation = match env
        .get(TOOL_CHILD_GENERATION_ENV)
        .and_then(|raw| raw.trim().parse::<u64>().ok())
    {
        Some(generation) if generation < (1 << 53) => generation,
        _ => {
            eprintln!("tool-child: invalid generation");
            exit(70);
            return Ok(());
        }
    };
    let channel = connect_channel(&env).await?;
    let controls = read_test_controls(&env);
    if controls.enabled(TestSwitch::ExitOnStart) {
        exit(1);
        return Ok(());
    }
    // Read once; from here no command this child starts may see them (the gated spawns pass their environment
    // explicitly).
    remember_tool_child_reentry(env.get(TOOL_CHILD_REENTRY_ENV).cloned());
    if scrub {
        scrub_process_env();
    }
    let sender: Arc<ChannelSender> = Arc::new(ChannelSender(channel.clone()));
    let gate = Arc::new(LaunchGate::new(sender.clone(), None, &env, None)?);
    let refs = env.get(TOOL_CHILD_REFS_ROOT_ENV).map(|root| {
        let reader_channel = channel.clone();
        HostCallRefs {
            writer: Arc::new(PayloadRefWriter::new(PayloadRefWriterOptions::new(
                root.clone(),
                generation,
            ))),
            reader: Arc::new(PayloadRefReader::new(PayloadRefReaderOptions {
                root: root.into(),
                generation,
                budget: None,
                send: Arc::new(move |control: RefControl| {
                    let _ = reader_channel.send(&ToolChildMessage::Ref(control));
                }),
            })),
        }
    });
    let host_calls = Arc::new(ChildHostCalls::new(sender.clone(), refs.clone()));
    let wakes = Arc::new(WakeAdmission::new());
    let wake_seq = Arc::new(AtomicU64::new(0));
    let wake: WakeFn = {
        let (channel, wakes) = (channel.clone(), wakes.clone());
        Arc::new(move |source: String, value: Value| {
            let (channel, wakes) = (channel.clone(), wakes.clone());
            let wake_id = format!("w{}", wake_seq.fetch_add(1, Ordering::SeqCst) + 1);
            async move {
                let bytes = serde_json::to_string(&value)
                    .map(|json| json.len())
                    .unwrap_or(4);
                wakes
                    .send(&wake_id, bytes)
                    .await
                    .map_err(|error| ToolChildJobError::new(error.code(), error.to_string()))?;
                let _ = channel.send(&ToolChildMessage::Child(ChildToServer::Wake {
                    wake_id,
                    source,
                    bytes: bytes as u64,
                    value: Some(value),
                }));
                Ok(())
            }
            .boxed()
        })
    };
    let job_kinds: Vec<String> = options.handlers.keys().cloned().collect();
    let session = Arc::new(Session {
        channel: channel.clone(),
        gate,
        host_calls: host_calls.clone(),
        refs,
        handlers: options.handlers,
        running: Mutex::new(HashMap::new()),
        streams: Mutex::new(HashMap::new()),
        generation,
        wake,
    });
    let shutdown_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let exit_on_message = exit.clone();
    {
        let (session, wakes, shutdown_requested) =
            (session.clone(), wakes.clone(), shutdown_requested.clone());
        let controls = controls.clone();
        let _unsubscribe = channel.on_message(move |message| {
            if controls.enabled(TestSwitch::BlockReader)
                && !reader_blocked.swap(true, Ordering::SeqCst)
            {
                // Test-only: the protocol loop stops reading for 30 s, so pings go unanswered.
                std::thread::sleep(Duration::from_secs(30));
            }
            if session.gate.handle(&message) {
                return;
            }
            if session.host_calls.handle(&message) {
                return;
            }
            let server = match &message {
                ToolChildMessage::Ref(control @ RefControl::RefAck { .. })
                | ToolChildMessage::Ref(control @ RefControl::RefNack { .. }) => {
                    if let Some(refs) = &session.refs {
                        let (writer, control) = (refs.writer.clone(), control.clone());
                        tokio::spawn(async move {
                            let _ = writer.handle(&control).await;
                        });
                    }
                    return;
                }
                ToolChildMessage::Server(server) => server,
                _ => return,
            };
            match server {
                ServerToChild::Ping { seq } => {
                    let _ = session
                        .channel
                        .send(&ToolChildMessage::Child(ChildToServer::Pong { seq: *seq }));
                }
                ServerToChild::Job { kind, .. } => {
                    if controls.value(TestSwitch::ExitMidJob).as_deref() == Some(kind.as_str()) {
                        exit_on_message(1);
                        return;
                    }
                    let (session, job) = (session.clone(), server.clone());
                    tokio::spawn(async move { run_job(session, job).await });
                }
                ServerToChild::WakeAck { wake_id } => wakes.ack(wake_id),
                ServerToChild::UpdateAck { job_id, seq } => {
                    let stream = lock(&session.streams).get(job_id).cloned();
                    if let Some(stream) = stream {
                        stream.acked(*seq);
                    }
                }
                ServerToChild::Cancel { job_id } => {
                    if controls.enabled(TestSwitch::IgnoreCancel) {
                        return;
                    }
                    if let Some(controller) = lock(&session.running).get(job_id) {
                        controller.abort();
                    }
                    let _ =
                        session
                            .channel
                            .send(&ToolChildMessage::Child(ChildToServer::CancelAck {
                                job_id: job_id.clone(),
                            }));
                }
                ServerToChild::Shutdown => {
                    shutdown_requested.store(true, Ordering::SeqCst);
                    session.channel.close();
                }
                _ => {}
            }
        });
    }
    if !controls.enabled(TestSwitch::HangBeforeReady) {
        let _ = channel.send(&ToolChildMessage::Child(ChildToServer::Hello {
            generation,
            pid: i64::from(std::process::id()),
            protocol: TOOL_CHILD_PROTOCOL_VERSION,
            job_kinds,
        }));
    }
    #[cfg(unix)]
    if controls.enabled(TestSwitch::StopSelf) {
        let _ =
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::STOP);
    }
    channel.closed().await;
    host_calls.close();
    for controller in lock(&session.running).values() {
        controller.abort();
    }
    if !shutdown_requested.load(Ordering::SeqCst) {
        orphaned_by_host(&env).await;
    }
    exit(0);
    Ok(())
}

async fn read_job_args(
    payload_ref: &PayloadRef,
    reader: Option<&Arc<PayloadRefReader>>,
    signal: &AbortSignal,
) -> Result<Value, HandlerError> {
    let Some(reader) = reader else {
        return Err(Box::new(ToolChildJobError::new(
            "tool_child_refs_unavailable",
            "a job sent its arguments by reference, but this child reads no references",
        )));
    };
    // The payload layer observes its own abort flag: it follows the job's cancel.
    let ref_signal = RefAbortSignal::new();
    let follow = {
        let (signal, ref_signal) = (signal.clone(), ref_signal.clone());
        tokio::spawn(async move {
            signal.cancelled().await;
            ref_signal.abort();
        })
    };
    let outcome = unpack_payload(payload_ref, reader, Some(&ref_signal)).await;
    follow.abort();
    Ok(outcome?)
}

async fn run_job(session: Arc<Session>, job: ServerToChild) {
    let ServerToChild::Job {
        job_id,
        kind,
        args,
        args_ref,
        ..
    } = job
    else {
        return;
    };
    let channel = &session.channel;
    let reader = session.refs.as_ref().map(|refs| &refs.reader);
    let handler = session.handlers.get(&kind).cloned();
    let duplicate = lock(&session.running).contains_key(&job_id);
    if (handler.is_none() || duplicate)
        && let (Some(args_ref), Some(reader)) = (&args_ref, reader)
    {
        reader.refuse(args_ref, crate::protocol::RefNackReason::Cancelled);
    }
    let Some(handler) = handler else {
        let _ = channel.send(&ToolChildMessage::Child(ChildToServer::JobError {
            job_id,
            code: "tool_child_unknown_kind".into(),
            message: format!("no handler for {kind}"),
        }));
        return;
    };
    if duplicate {
        let _ = channel.send(&ToolChildMessage::Child(ChildToServer::JobRefused {
            job_id,
            code: "tool_child_duplicate_job".into(),
        }));
        return;
    }
    let controller = AbortSignal::default();
    lock(&session.running).insert(job_id.clone(), controller.clone());
    let outcome: Result<Option<Value>, HandlerError> = async {
        let host_call: HostCallFn = {
            let (host_calls, job_id, controller) = (
                session.host_calls.clone(),
                job_id.clone(),
                controller.clone(),
            );
            Arc::new(move |target, options| {
                let (host_calls, job_id) = (host_calls.clone(), job_id.clone());
                // The signal of a host call that outlives its job's cancel.
                let signal = if options.survives_cancel {
                    AbortSignal::default()
                } else {
                    controller.clone()
                };
                async move { host_calls.call(&job_id, target, &signal).await }.boxed()
            })
        };
        // Large arguments arrive by a server-written reference: acked (now this child's), consumed, deleted.
        let args = match &args_ref {
            None => args.unwrap_or(Value::Null),
            Some(args_ref) => read_job_args(args_ref, reader, &controller).await?,
        };
        let stream = Arc::new(JobUpdates::new(JobUpdatesOptions {
            job_id: job_id.clone(),
            channel: Arc::new(ChannelSender(channel.clone())),
            writer: session.refs.as_ref().map(|refs| refs.writer.clone()),
            window: None,
        }));
        lock(&session.streams).insert(job_id.clone(), stream.clone());
        let update: UpdateFn = {
            let stream = stream.clone();
            Arc::new(move |value| {
                let update = stream.update(value);
                async move {
                    update.await.map_err(|error| {
                        ToolChildJobError::new("tool_child_job_failed", error.to_string())
                    })
                }
                .boxed()
            })
        };
        handler(
            args,
            ToolChildJobContext {
                job_id: job_id.clone(),
                signal: controller.clone(),
                channel: channel.clone(),
                gate: session.gate.clone(),
                host_call,
                update,
                generation: session.generation,
                wake: session.wake.clone(),
            },
        )
        .await
    }
    .await;
    match outcome {
        Ok(value) => {
            let _ = channel.send(&ToolChildMessage::Child(ChildToServer::Result {
                job_id: job_id.clone(),
                value: value.unwrap_or(Value::Null),
            }));
        }
        Err(error) => {
            let code = match error.downcast_ref::<ToolChildJobError>() {
                Some(typed) => typed.code.clone(),
                None if controller.is_aborted() => "tool_child_cancelled".to_owned(),
                None => "tool_child_job_failed".to_owned(),
            };
            let _ = channel.send(&ToolChildMessage::Child(ChildToServer::JobError {
                job_id: job_id.clone(),
                code,
                message: error.to_string(),
            }));
        }
    }
    lock(&session.running).remove(&job_id);
    let stream = lock(&session.streams).remove(&job_id);
    if let Some(stream) = stream {
        stream.end();
    }
}

/// The host is gone without asking this child to stop (a crash, SIGKILL, an exit that never closed the session): nobody
/// is left to sweep. Sweep the owned groups the ledger names, then end this child's own group - the inherited commands
/// share it - with this process in it.
async fn orphaned_by_host(env: &ProcessEnv) {
    if let Some(ledger) = env.get(TOOL_CHILD_LEDGER_ENV) {
        let _ = sweep_ledger(Path::new(ledger)).await;
    }
    #[cfg(unix)]
    {
        // Not a group leader (a test harness started this entry in-process): the kill finds nothing to end.
        let _ = rustix::process::kill_process_group(
            rustix::process::getpid(),
            rustix::process::Signal::KILL,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{TOOL_CHILD_CHANNEL_ENV, open_channel_listener};
    use std::sync::atomic::AtomicI32;
    use tokio::sync::mpsc;

    const BOUND: Duration = Duration::from_secs(20);

    fn echo_handler() -> ToolChildJobHandler {
        Arc::new(|args, _context| async move { Ok(Some(args)) }.boxed())
    }

    fn failing_handler() -> ToolChildJobHandler {
        Arc::new(|_args, _context| {
            async move {
                Err::<Option<Value>, HandlerError>(Box::new(ToolChildJobError::new(
                    "typed_failure",
                    "nope",
                )))
            }
            .boxed()
        })
    }

    /// Starts `run_tool_child` in this process against a fresh listener; frames the server hears arrive on `frames`.
    async fn harness() -> (
        ToolChildChannel,
        mpsc::UnboundedReceiver<ToolChildMessage>,
        Arc<AtomicI32>,
        tokio::task::JoinHandle<()>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let listener = open_channel_listener(dir.path(), 1)
            .await
            .expect("listener");
        let mut env: ProcessEnv = listener.env.clone();
        assert!(env.contains_key(TOOL_CHILD_CHANNEL_ENV));
        env.insert(TOOL_CHILD_GENERATION_ENV.into(), "1".into());
        let exited = Arc::new(AtomicI32::new(-1));
        let mut handlers = BTreeMap::new();
        handlers.insert("echo".to_owned(), echo_handler());
        handlers.insert("fail".to_owned(), failing_handler());
        let exit_code = exited.clone();
        let entry = tokio::spawn(async move {
            let _ = run_tool_child(ToolChildEntryOptions {
                handlers,
                env: Some(env),
                exit: Some(Arc::new(move |code| {
                    exit_code.store(code, Ordering::SeqCst)
                })),
            })
            .await;
        });
        let server = tokio::time::timeout(BOUND, listener.accepted())
            .await
            .expect("accept in time")
            .expect("accepted");
        let (tx, rx) = mpsc::unbounded_channel();
        let _unsubscribe = server.on_message(move |message| {
            let _ = tx.send(message);
        });
        (server, rx, exited, entry, dir)
    }

    async fn next(rx: &mut mpsc::UnboundedReceiver<ToolChildMessage>) -> ToolChildMessage {
        tokio::time::timeout(BOUND, rx.recv())
            .await
            .expect("a frame in time")
            .expect("channel open")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn says_hello_with_the_sorted_job_kinds_answers_pings_runs_jobs_and_exits_on_shutdown() {
        let (server, mut rx, exited, entry, _dir) = harness().await;
        match next(&mut rx).await {
            ToolChildMessage::Child(ChildToServer::Hello {
                generation,
                protocol,
                job_kinds,
                ..
            }) => {
                assert_eq!(generation, 1);
                assert_eq!(protocol, TOOL_CHILD_PROTOCOL_VERSION);
                assert_eq!(job_kinds, vec!["echo".to_owned(), "fail".to_owned()]);
            }
            other => panic!("expected hello, got {other:?}"),
        }
        let send = |message: ServerToChild| {
            let _ = server.send(&ToolChildMessage::Server(message));
        };
        send(ServerToChild::Ping { seq: 7 });
        assert_eq!(
            next(&mut rx).await,
            ToolChildMessage::Child(ChildToServer::Pong { seq: 7 })
        );
        send(ServerToChild::Job {
            job_id: "j1".into(),
            kind: "echo".into(),
            parent_job_id: None,
            args: Some(serde_json::json!({"a": 1})),
            args_ref: None,
        });
        assert_eq!(
            next(&mut rx).await,
            ToolChildMessage::Child(ChildToServer::Result {
                job_id: "j1".into(),
                value: serde_json::json!({"a": 1})
            })
        );
        send(ServerToChild::Job {
            job_id: "j2".into(),
            kind: "fail".into(),
            parent_job_id: None,
            args: None,
            args_ref: None,
        });
        assert_eq!(
            next(&mut rx).await,
            ToolChildMessage::Child(ChildToServer::JobError {
                job_id: "j2".into(),
                code: "typed_failure".into(),
                message: "nope".into()
            })
        );
        send(ServerToChild::Job {
            job_id: "j3".into(),
            kind: "missing".into(),
            parent_job_id: None,
            args: None,
            args_ref: None,
        });
        assert_eq!(
            next(&mut rx).await,
            ToolChildMessage::Child(ChildToServer::JobError {
                job_id: "j3".into(),
                code: "tool_child_unknown_kind".into(),
                message: "no handler for missing".into()
            })
        );
        send(ServerToChild::Shutdown);
        tokio::time::timeout(BOUND, entry)
            .await
            .expect("entry ends in time")
            .expect("entry task");
        assert_eq!(exited.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_an_invalid_generation_with_exit_70() {
        let exited = Arc::new(AtomicI32::new(-1));
        let exit_code = exited.clone();
        let mut env = ProcessEnv::new();
        env.insert(TOOL_CHILD_GENERATION_ENV.into(), "nope".into());
        let result = run_tool_child(ToolChildEntryOptions {
            handlers: BTreeMap::new(),
            env: Some(env),
            exit: Some(Arc::new(move |code| {
                exit_code.store(code, Ordering::SeqCst)
            })),
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(exited.load(Ordering::SeqCst), 70);
    }
}
