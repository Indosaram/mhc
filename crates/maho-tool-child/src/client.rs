//! senpi `modes/rpc/tool-child/client.ts` (pin 26acd9353): the server's handle on one session's tool child (senpi#3566,
//! plan todo 4).
//!
//! Every spawn is a new generation with its own channel, so a frame from an older generation can never settle a job of
//! a newer one. Each job settles exactly once: by its result, its error, or the child's exit, whichever comes first.
//! When the child dies every pending job of that generation fails with `tool_child_exited`; nothing is re-run here.
//!
//! Rust shape: the client is cheap to share by reference (`&self` methods over an `Arc` core). A job's promise is a
//! oneshot; `exited` is a shared future fed by a task that waits on the child process.
use std::any::Any;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::{BoxFuture, FutureExt, Shared};
use maho_ext_api::AbortSignal;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::channel::{ToolChildChannel, open_channel_listener};
use crate::child_containment::{ChildContainment, ContainmentHost, contain_child};
use crate::client_barrier::{
    GenerationBarrier, GenerationBarrierOptions, create_generation_barrier,
};
use crate::descendants::generation_ledger_path;
use crate::entry::{ChannelSender, TOOL_CHILD_PROTOCOL_VERSION};
use crate::host_calls_server::{
    HostCallContext, HostCallContextOf, HostCallHandler, HostCallRefs, HostCallScope,
    ServerHostCalls,
};
use crate::job_policy::{PolicyOptions, policy_for};
use crate::job_supervisor::{
    ChildSignal, JobSupervisor, SupervisedChild, SupervisorTimers, TrackOptions,
};
use crate::launch::{
    SpawnToolChildOptions, ToolChildCommand, spawn_tool_child, tool_child_command,
};
use crate::launch_env_names::{ProcessEnv, process_env};
use crate::orphan_sweep::{record_generation_child, sweep_stale_generations};
use crate::payload_ref::{
    PayloadRefReader, PayloadRefReaderOptions, PayloadRefWriter, PayloadRefWriterOptions,
};
use crate::payload_wire::unpack_payload;
use crate::protocol::{
    ChildToServer, RefControl, RefNackReason, ServerToChild, TOOL_CHILD_LIMITS, ToolChildMessage,
};

type Signal = Shared<BoxFuture<'static, ()>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolChildState {
    None,
    Spawning,
    Live,
    Retiring,
    Dead,
}

impl ToolChildState {
    /// The upstream state string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Spawning => "spawning",
            Self::Live => "live",
            Self::Retiring => "retiring",
            Self::Dead => "dead",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolChildStatusState {
    Spawned,
    Exited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolChildStatus {
    pub state: ToolChildStatusState,
    pub generation: u64,
    /// -1 when the child never had a pid.
    pub pid: i64,
}

/// A child's `wake`: something happened on it outside any job's lifetime, for the job kind named by `source`.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolChildWake {
    pub source: String,
    pub value: Option<Value>,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ToolChildError {
    pub code: String,
    /// `<code>: <message>`.
    pub message: String,
}

impl ToolChildError {
    /// The upstream `this.name`.
    pub const NAME: &'static str = "ToolChildError";

    pub fn new(code: &str, message: impl AsRef<str>) -> Self {
        Self {
            code: code.to_owned(),
            message: format!("{code}: {}", message.as_ref()),
        }
    }

    pub fn name(&self) -> &'static str {
        Self::NAME
    }
}

/// `NodeJS.ProcessEnv | (() => NodeJS.ProcessEnv)`: a function is read at every spawn.
#[derive(Clone)]
pub enum ToolChildEnv {
    Fixed(ProcessEnv),
    Dynamic(Arc<dyn Fn() -> ProcessEnv + Send + Sync>),
}

/// Observes one cancel acknowledgement by job id.
pub type CancelAckFn = dyn Fn(&str) + Send + Sync;

#[derive(Default)]
pub struct ToolChildClientOptions {
    pub dir: PathBuf,
    pub cwd: PathBuf,
    /// `None` is this process's environment.
    pub env: Option<ToolChildEnv>,
    pub command: Option<ToolChildCommand>,
    /// Default 10 000.
    pub ready_ms: Option<u64>,
    /// Default 2 000.
    pub shutdown_ms: Option<u64>,
    pub on_status: Option<Arc<dyn Fn(ToolChildStatus) + Send + Sync>>,
    /// Tests inject a fake clock for the job supervisor.
    pub timers: Option<Arc<dyn SupervisorTimers>>,
    /// Serves the child's reverse-bridge calls (tool, completion, reserved, list_tools); unset = every call fails.
    pub on_host_call: Option<HostCallHandler>,
    /// Receives the child's wakes, in order; each is acknowledged once delivered.
    pub on_wake: Option<Arc<dyn Fn(ToolChildWake) + Send + Sync>>,
    /// Observes the child's cancel acknowledgements (tests and status).
    pub on_cancel_ack: Option<Arc<CancelAckFn>>,
    /// Read for `SENPI_TOOL_CHILD_DEADLINE_<KIND>_MS` fixture overrides.
    pub policy_env: Option<ProcessEnv>,
    /// Tests inject the platform and kernel; production uses this host's.
    pub containment_host: Option<Arc<ContainmentHost>>,
}

/// The options of one `run`.
#[derive(Default)]
pub struct RunOptions {
    pub idempotent: Option<bool>,
    pub deadline_ms: Option<f64>,
    pub host_context: HostCallContext,
    pub on_update: Option<Arc<dyn Fn(Value) + Send + Sync>>,
}

struct Pending {
    /// Settles the job's promise (and releases its supervisor, host-call and context state).
    finish: Box<dyn FnOnce(Result<Value, ToolChildError>) + Send>,
    /// Receives the job's streamed updates in order; absent when the caller does not stream.
    on_update: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    /// Every update received so far, delivered and acknowledged in order; a result settles after it.
    delivered: Signal,
}

type PendingMap = Arc<Mutex<HashMap<String, Pending>>>;

struct Live {
    generation: u64,
    pid: Option<u32>,
    channel: ToolChildChannel,
    exited: Signal,
    pending: PendingMap,
    pongs: Mutex<HashMap<u64, oneshot::Sender<()>>>,
    barrier: GenerationBarrier,
    supervisor: JobSupervisor,
    host_calls: ServerHostCalls,
    refs: HostCallRefs,
    /// The child's manifest from its `hello`: the job kinds it runs.
    job_kinds: BTreeSet<String>,
    containment: Arc<ChildContainment>,
    /// Completes once `on_exit` has run its synchronous part (its teardown is then in `last_ended`).
    exit_handled: Signal,
    exit_handled_tx: Mutex<Option<oneshot::Sender<()>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn settle(pending: &Mutex<HashMap<String, Pending>>, job_id: &str, apply: impl FnOnce(Pending)) {
    let found = lock(pending).remove(job_id);
    if let Some(found) = found {
        apply(found);
    }
}

/// Runs `then` once every update the job sent before it has been delivered (at once when there is none).
fn after_updates(
    pending: &Mutex<HashMap<String, Pending>>,
    job_id: &str,
    then: impl FnOnce() + Send + 'static,
) {
    let delivered = lock(pending)
        .get(job_id)
        .map(|found| found.delivered.clone());
    let Some(delivered) = delivered else { return };
    tokio::spawn(async move {
        delivered.await;
        then();
    });
}

/// Sends a job: inline at or below the reference threshold, otherwise by a server-written reference the child acks and
/// then owns. A job settled (cancelled) while its reference was written never reaches the child, and its reference is
/// released at once.
fn send_job(live: &Arc<Live>, job_id: String, kind: String, args: Option<Value>) {
    let json = match &args {
        None => None,
        Some(value) => serde_json::to_vec(value).ok(),
    };
    let inline = match &json {
        None => true,
        Some(bytes) => bytes.len() <= TOOL_CHILD_LIMITS.ref_threshold,
    };
    if inline {
        let _ = live
            .channel
            .send(&ToolChildMessage::Server(ServerToChild::Job {
                job_id,
                kind,
                parent_job_id: None,
                args,
                args_ref: None,
            }));
        return;
    }
    let live = live.clone();
    tokio::spawn(async move {
        let bytes = json.unwrap_or_default();
        match live.refs.writer.create(&bytes).await {
            Ok(payload_ref) => {
                if lock(&live.pending).contains_key(&job_id) {
                    let _ = live
                        .channel
                        .send(&ToolChildMessage::Server(ServerToChild::Job {
                            job_id,
                            kind,
                            parent_job_id: None,
                            args: None,
                            args_ref: Some(payload_ref),
                        }));
                } else {
                    let _ = live
                        .refs
                        .writer
                        .handle(&RefControl::RefNack {
                            ref_id: payload_ref.ref_id,
                            reason: RefNackReason::Cancelled,
                        })
                        .await;
                }
            }
            Err(error) => settle(&live.pending, &job_id, |pending| {
                (pending.finish)(Err(ToolChildError::new(
                    "tool_child_args_unwritable",
                    error.to_string(),
                )));
            }),
        }
    });
}

struct State {
    state: ToolChildState,
    generation: u64,
    next_job: u64,
    next_ping: u64,
    live: Option<Arc<Live>>,
    last_ended: Signal,
}

struct Inner {
    options: ToolChildClientOptions,
    state: Mutex<State>,
    host_contexts: Arc<Mutex<HashMap<String, HostCallContext>>>,
}

pub struct ToolChildClient {
    inner: Arc<Inner>,
}

impl ToolChildClient {
    pub fn new(options: ToolChildClientOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                options,
                state: Mutex::new(State {
                    state: ToolChildState::None,
                    generation: 0,
                    next_job: 0,
                    next_ping: 0,
                    live: None,
                    last_ended: futures::future::ready(()).boxed().shared(),
                }),
                host_contexts: Arc::new(Mutex::new(HashMap::new())),
            }),
        }
    }

    pub fn state(&self) -> ToolChildState {
        lock(&self.inner.state).state
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.inner.options.dir
    }

    /// The live child's manifest (its `hello`'s job kinds); empty when no child is live.
    pub fn job_kinds(&self) -> Vec<String> {
        match &lock(&self.inner.state).live {
            Some(live) => live.job_kinds.iter().cloned().collect(),
            None => Vec::new(),
        }
    }

    pub fn generation(&self) -> u64 {
        lock(&self.inner.state).generation
    }

    /// Resolves once the current or last generation's teardown (group sweep, ledger removal) finished.
    pub async fn ended(&self) {
        let last = lock(&self.inner.state).last_ended.clone();
        last.await;
    }

    pub fn pid(&self) -> Option<u32> {
        lock(&self.inner.state)
            .live
            .as_ref()
            .and_then(|live| live.pid)
    }

    pub async fn start(&self) -> Result<(), ToolChildError> {
        let generation = {
            let mut state = lock(&self.inner.state);
            if matches!(state.state, ToolChildState::Spawning | ToolChildState::Live) {
                return Err(ToolChildError::new(
                    "tool_child_busy",
                    format!("already {}", state.state.as_str()),
                ));
            }
            state.generation += 1;
            state.state = ToolChildState::Spawning;
            state.generation
        };
        match bootstrap(&self.inner, generation).await {
            Ok(()) => Ok(()),
            Err(error) => {
                // Whatever failed (the listener, the spawn, the hello), the client never stays "spawning": the next job
                // retries.
                let mut state = lock(&self.inner.state);
                if state.state == ToolChildState::Spawning {
                    state.state = ToolChildState::Dead;
                }
                Err(error)
            }
        }
    }

    pub async fn run(
        &self,
        kind: &str,
        args: Option<Value>,
        signal: Option<AbortSignal>,
        options: RunOptions,
    ) -> Result<Value, ToolChildError> {
        let (live, job_id) = {
            let mut state = lock(&self.inner.state);
            let live = match (&state.live, state.state) {
                (Some(live), ToolChildState::Live) => live.clone(),
                _ => {
                    return Err(ToolChildError::new(
                        "tool_child_unavailable",
                        state.state.as_str(),
                    ));
                }
            };
            if !live.job_kinds.contains(kind) {
                let known: Vec<&str> = live.job_kinds.iter().map(String::as_str).collect();
                return Err(ToolChildError::new(
                    "tool_child_unknown_kind",
                    format!("the child runs [{}], not {kind}", known.join(", ")),
                ));
            }
            state.next_job += 1;
            (live, format!("j{}", state.next_job))
        };
        if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return Err(ToolChildError::new(
                "tool_child_cancelled",
                "cancelled before it started",
            ));
        }
        if let Some(context) = options.host_context.clone() {
            lock(&self.inner.host_contexts).insert(job_id.clone(), Some(context));
        }
        let policy = policy_for(
            kind,
            &PolicyOptions {
                idempotent: options.idempotent,
                deadline_ms: options.deadline_ms,
                env: Some(
                    self.inner
                        .options
                        .policy_env
                        .clone()
                        .unwrap_or_else(process_env),
                ),
            },
        );
        let (tx, rx) = oneshot::channel();
        let finish: Box<dyn FnOnce(Result<Value, ToolChildError>) + Send> = {
            let (live, contexts, job_id) = (
                live.clone(),
                self.inner.host_contexts.clone(),
                job_id.clone(),
            );
            Box::new(move |result| {
                live.supervisor.settle(&job_id);
                live.host_calls.end_job(&job_id);
                lock(&contexts).remove(&job_id);
                let _ = tx.send(result);
            })
        };
        lock(&live.pending).insert(
            job_id.clone(),
            Pending {
                finish,
                on_update: options.on_update,
                delivered: futures::future::ready(()).boxed().shared(),
            },
        );
        let backstop_ms = (policy.backstop_ms)(args.as_ref());
        let reject = {
            let (pending, job_id) = (live.pending.clone(), job_id.clone());
            Arc::new(move |error: crate::job_supervisor::JobSupervisorError| {
                settle(&pending, &job_id, |found| {
                    (found.finish)(Err(ToolChildError {
                        code: error.code,
                        message: error.message,
                    }));
                });
            })
        };
        live.supervisor.track(
            &job_id,
            TrackOptions {
                reject,
                backstop_ms,
                user_text: policy.user_text.clone(),
                signal,
            },
        );
        send_job(&live, job_id, kind.to_owned(), args);
        rx.await.unwrap_or_else(|_| {
            Err(ToolChildError::new(
                "tool_child_exited",
                "the job was dropped",
            ))
        })
    }

    pub async fn ping(&self) -> Result<u64, ToolChildError> {
        let (live, seq) = {
            let mut state = lock(&self.inner.state);
            let Some(live) = state.live.clone() else {
                return Err(ToolChildError::new(
                    "tool_child_unavailable",
                    state.state.as_str(),
                ));
            };
            state.next_ping += 1;
            (live, state.next_ping)
        };
        let (tx, rx) = oneshot::channel();
        lock(&live.pongs).insert(seq, tx);
        let _ = live
            .channel
            .send(&ToolChildMessage::Server(ServerToChild::Ping { seq }));
        let _ = rx.await;
        Ok(seq)
    }

    pub async fn close(&self) {
        let live = lock(&self.inner.state).live.clone();
        // A child that already died may still be tearing down (ledger, launch log): close() means all of it is finished.
        let Some(live) = live else {
            self.ended().await;
            return;
        };
        lock(&self.inner.state).state = ToolChildState::Retiring;
        let _ = live
            .channel
            .send(&ToolChildMessage::Server(ServerToChild::Shutdown));
        let shutdown = Duration::from_millis(self.inner.options.shutdown_ms.unwrap_or(2_000));
        let graceful = tokio::select! {
            () = live.exited.clone() => true,
            () = tokio::time::sleep(shutdown) => false,
        };
        if !graceful {
            live.containment.kill();
        }
        live.exited.clone().await;
        live.exit_handled.clone().await;
        self.ended().await;
    }
}

/// The channel's first frame; the subscription ends with it (frames after it belong to the live client's handler).
fn first_message(channel: &ToolChildChannel) -> oneshot::Receiver<ToolChildMessage> {
    type Unsubscribe = Box<dyn FnOnce() + Send>;
    #[derive(Default)]
    struct Slot {
        done: bool,
        unsubscribe: Option<Unsubscribe>,
    }
    let (tx, rx) = oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(tx)));
    let slot: Arc<Mutex<Slot>> = Arc::new(Mutex::new(Slot::default()));
    let listener_slot = slot.clone();
    let unsubscribe = channel.on_message(move |message| {
        let first = lock(&sender).take();
        let Some(first) = first else { return };
        let _ = first.send(message);
        let mut slot = lock(&listener_slot);
        slot.done = true;
        if let Some(unsubscribe) = slot.unsubscribe.take() {
            drop(slot);
            unsubscribe();
        }
    });
    // on_message replays a queued frame synchronously, before it returned the unsubscribe function.
    let mut slot = lock(&slot);
    if slot.done {
        drop(slot);
        unsubscribe();
    } else {
        slot.unsubscribe = Some(unsubscribe);
    }
    rx
}

struct Supervised {
    channel: ToolChildChannel,
    containment: Arc<ChildContainment>,
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "only POSIX signals the child's process group")
    )]
    pid: Option<u32>,
}

impl SupervisedChild for Supervised {
    fn send_cancel(&self, job_id: &str) {
        let _ = self
            .channel
            .send(&ToolChildMessage::Server(ServerToChild::Cancel {
                job_id: job_id.to_owned(),
            }));
    }

    fn send_ping(&self, seq: u64) {
        let _ = self
            .channel
            .send(&ToolChildMessage::Server(ServerToChild::Ping { seq }));
    }

    fn signal(&self, signal: ChildSignal) {
        // Windows has no graceful group signal: the Job Object is the only tree-wide stop.
        if signal == ChildSignal::Sigkill || cfg!(windows) {
            self.containment.kill();
        } else {
            #[cfg(unix)]
            if let Some(group) = self
                .pid
                .and_then(|pid| i32::try_from(pid).ok())
                .and_then(rustix::process::Pid::from_raw)
            {
                // The child's group may already be gone.
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
            }
        }
    }
}

fn spawn_failed(error: impl std::fmt::Display) -> ToolChildError {
    ToolChildError::new(
        "tool_child_bootstrap_failed",
        format!("spawn_failed: {error}"),
    )
}

async fn bootstrap(inner: &Arc<Inner>, generation: u64) -> Result<(), ToolChildError> {
    let options = &inner.options;
    // A crashed predecessor (host or child) may have left owned groups and a ledger this generation would reuse.
    let last_ended = lock(&inner.state).last_ended.clone();
    last_ended.await;
    let _ = sweep_stale_generations(&options.dir).await;
    let listener = open_channel_listener(&options.dir, generation)
        .await
        .map_err(spawn_failed)?;
    let refs_root = options.dir.join("refs");
    let env = match &options.env {
        Some(ToolChildEnv::Dynamic(read)) => read(),
        Some(ToolChildEnv::Fixed(env)) => env.clone(),
        None => process_env(),
    };
    let command = match &options.command {
        Some(command) => command.clone(),
        None => match tool_child_command() {
            Ok(command) => command,
            Err(error) => {
                let _ = listener.close().await;
                return Err(spawn_failed(error));
            }
        },
    };
    let spawned = u32::try_from(generation)
        .map_err(std::io::Error::other)
        .and_then(|numbered| {
            spawn_tool_child(SpawnToolChildOptions {
                generation: numbered,
                channel_env: listener.env.clone(),
                env,
                command,
                cwd: options.cwd.clone(),
                refs_root: refs_root.to_string_lossy().into_owned(),
                ledger_path: generation_ledger_path(&options.dir, generation)
                    .to_string_lossy()
                    .into_owned(),
            })
        });
    let mut child = match spawned {
        Ok(child) => child,
        // A spawn that failed (ENOENT, a missing cwd) has no pid and no process to stop, so nothing is signalled.
        Err(error) => {
            let _ = listener.close().await;
            return Err(spawn_failed(error));
        }
    };
    let pid = child.id();
    // Nobody reads the child's stdout / stderr; they are drained so a chatty job can never block on a full pipe.
    if let Some(mut stdout) = child.stdout.take() {
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
        });
    }
    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        });
    }
    let (exit_tx, exit_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = child.wait().await;
        let _ = exit_tx.send(());
    });
    let exited: Signal = async move {
        let _ = exit_rx.await;
    }
    .boxed()
    .shared();
    let default_host;
    let host: &ContainmentHost = match &options.containment_host {
        Some(host) => host,
        None => {
            default_host = ContainmentHost::default();
            &default_host
        }
    };
    let containment = match contain_child(pid, host) {
        Ok(containment) => Arc::new(containment),
        Err(error) => {
            let _ = listener.close().await;
            return Err(ToolChildError::new(&error.code, &error.message));
        }
    };
    // Marks this generation live while its child runs: another start on the same session never sweeps it.
    if let Some(pid) = pid {
        let _ = record_generation_child(&options.dir, generation, pid).await;
    }
    let hello = {
        let listener = &listener;
        async move {
            let channel = listener.accepted().await.ok()?;
            let message = first_message(&channel).await.ok()?;
            Some((channel, message))
        }
    };
    let outcome = tokio::select! {
        found = hello => found.map(Ok).unwrap_or(Err("exited")),
        () = exited.clone() => Err("exited"),
        () = tokio::time::sleep(Duration::from_millis(options.ready_ms.unwrap_or(10_000))) => Err("timeout"),
    };
    let _ = listener.close().await;
    let (channel, message) = match outcome {
        Ok(found) => found,
        Err(why) => {
            containment.kill();
            containment.close();
            lock(&inner.state).state = ToolChildState::Dead;
            return Err(ToolChildError::new(
                "tool_child_bootstrap_failed",
                if why == "timeout" {
                    "ready_timeout"
                } else {
                    "exited_before_ready"
                },
            ));
        }
    };
    let job_kinds = match message {
        ToolChildMessage::Child(ChildToServer::Hello {
            generation: said,
            protocol,
            job_kinds,
            ..
        }) if said == generation && protocol == TOOL_CHILD_PROTOCOL_VERSION => job_kinds,
        _ => {
            containment.kill();
            containment.close();
            lock(&inner.state).state = ToolChildState::Dead;
            return Err(ToolChildError::new(
                "tool_child_bootstrap_failed",
                "version_mismatch",
            ));
        }
    };
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let barrier = create_generation_barrier(GenerationBarrierOptions {
        dir: options.dir.clone(),
        generation,
        channel: Arc::new(channel.clone()),
        confirm_member: containment.has_is_member().then(|| {
            let containment = containment.clone();
            Arc::new(move |pid: i64| {
                u64::try_from(pid)
                    .ok()
                    .and_then(|pid| containment.is_member(pid))
                    .unwrap_or(true)
            }) as Arc<dyn Fn(i64) -> bool + Send + Sync>
        }),
        hold_ack: None,
        on_violation: {
            let (pending, containment) = (pending.clone(), containment.clone());
            Arc::new(move |violation| {
                let failure = ToolChildError::new(
                    violation.code.as_str(),
                    format!("{}: {}", violation.launch_id, violation.reason.as_str()),
                );
                if let Some(job_id) = &violation.job_id {
                    settle(&pending, job_id, |found| (found.finish)(Err(failure)));
                }
                containment.kill();
            })
        },
    });
    let supervisor = JobSupervisor::new(
        Arc::new(Supervised {
            channel: channel.clone(),
            containment: containment.clone(),
            pid,
        }),
        options.timers.clone(),
    );
    let refs = HostCallRefs {
        writer: Arc::new(PayloadRefWriter::new(PayloadRefWriterOptions::new(
            refs_root.clone(),
            generation,
        ))),
        reader: Arc::new(PayloadRefReader::new(PayloadRefReaderOptions {
            root: refs_root,
            generation,
            budget: None,
            send: {
                let channel = channel.clone();
                Arc::new(move |control: RefControl| {
                    let _ = channel.send(&ToolChildMessage::Ref(control));
                })
            },
        })),
    };
    let handler: HostCallHandler = options.on_host_call.clone().unwrap_or_else(|| {
        Arc::new(|request| {
            let label = request.name.clone().unwrap_or_else(|| {
                serde_json::to_value(request.kind)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default()
            });
            async move { Err(format!("no host-call handler for {label}")) }.boxed()
        })
    });
    let context_of: HostCallContextOf = {
        let contexts = inner.host_contexts.clone();
        Arc::new(move |job_id| -> Option<Arc<dyn Any + Send + Sync>> {
            lock(&contexts).get(job_id).cloned().flatten()
        })
    };
    let host_calls = ServerHostCalls::new(
        Arc::new(ChannelSender(channel.clone())),
        handler,
        Some(context_of),
        Some(refs.clone()),
        Some(HostCallScope {
            exited: exited.clone(),
            job_live: {
                let pending = pending.clone();
                Arc::new(move |job_id| lock(&pending).contains_key(job_id))
            },
        }),
    );
    let (handled_tx, handled_rx) = oneshot::channel::<()>();
    let live = Arc::new(Live {
        generation,
        pid,
        channel: channel.clone(),
        exited: exited.clone(),
        pending,
        pongs: Mutex::new(HashMap::new()),
        barrier,
        supervisor,
        host_calls,
        refs,
        job_kinds: job_kinds.into_iter().collect(),
        containment,
        exit_handled: async move {
            let _ = handled_rx.await;
        }
        .boxed()
        .shared(),
        exit_handled_tx: Mutex::new(Some(handled_tx)),
    });
    {
        let mut state = lock(&inner.state);
        state.live = Some(live.clone());
        state.state = ToolChildState::Live;
    }
    {
        let (inner, live) = (inner.clone(), live.clone());
        // The listener lives as long as the channel: its unsubscribe handle is intentionally dropped.
        let _unsubscribe = channel.on_message(move |frame| on_frame(&inner, &live, frame));
    }
    {
        let (inner, live) = (inner.clone(), live.clone());
        tokio::spawn(async move {
            live.exited.clone().await;
            on_exit(&inner, &live);
        });
    }
    if let Some(on_status) = &options.on_status {
        on_status(ToolChildStatus {
            state: ToolChildStatusState::Spawned,
            generation,
            pid: pid.map_or(-1, i64::from),
        });
    }
    Ok(())
}

fn on_frame(inner: &Arc<Inner>, live: &Arc<Live>, frame: ToolChildMessage) {
    let current = lock(&inner.state)
        .live
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(current, live));
    if !current {
        return;
    }
    if live.barrier.handle(&frame) {
        return;
    }
    if live.host_calls.handle(&frame) {
        return;
    }
    let child = match frame {
        ToolChildMessage::Ref(control) => {
            let writer = live.refs.writer.clone();
            tokio::spawn(async move {
                let _ = writer.handle(&control).await;
            });
            return;
        }
        ToolChildMessage::Child(child) => child,
        ToolChildMessage::Server(_) => return,
    };
    match child {
        ChildToServer::Update {
            job_id,
            seq,
            value,
            value_ref,
        } => on_update(live, job_id, seq, value, value_ref),
        ChildToServer::Result { job_id, value } => {
            let (pending, id) = (live.pending.clone(), job_id.clone());
            after_updates(&live.pending, &job_id, move || {
                settle(&pending, &id, |found| (found.finish)(Ok(value)));
            });
        }
        ChildToServer::JobError {
            job_id,
            code,
            message,
        } => {
            let (pending, id) = (live.pending.clone(), job_id.clone());
            after_updates(&live.pending, &job_id, move || {
                settle(&pending, &id, |found| {
                    (found.finish)(Err(ToolChildError::new(&code, message)))
                });
            });
        }
        ChildToServer::JobRefused { job_id, code } => {
            let (pending, id) = (live.pending.clone(), job_id.clone());
            after_updates(&live.pending, &job_id, move || {
                settle(&pending, &id, |found| {
                    (found.finish)(Err(ToolChildError::new(&code, "refused")))
                });
            });
        }
        ChildToServer::Wake {
            wake_id,
            source,
            value,
            ..
        } => {
            if let Some(on_wake) = &inner.options.on_wake {
                on_wake(ToolChildWake {
                    source,
                    value,
                    generation: live.generation,
                });
            }
            let _ = live
                .channel
                .send(&ToolChildMessage::Server(ServerToChild::WakeAck {
                    wake_id,
                }));
        }
        ChildToServer::CancelAck { job_id } => {
            live.supervisor.cancel_acked(&job_id);
            if let Some(on_cancel_ack) = &inner.options.on_cancel_ack {
                on_cancel_ack(&job_id);
            }
        }
        ChildToServer::Pong { seq } => {
            live.supervisor.pong_received();
            let done = lock(&live.pongs).remove(&seq);
            if let Some(done) = done {
                let _ = done.send(());
            }
        }
        _ => {}
    }
}

/// Delivers one update after the job's earlier ones (reading a ref first), then returns its credit.
fn on_update(
    live: &Arc<Live>,
    job_id: String,
    seq: u64,
    value: Option<Value>,
    value_ref: Option<crate::protocol::PayloadRef>,
) {
    let mut pending = lock(&live.pending);
    let Some(found) = pending.get_mut(&job_id) else {
        drop(pending);
        if let Some(value_ref) = value_ref {
            live.refs
                .reader
                .refuse(&value_ref, RefNackReason::Cancelled);
        }
        return;
    };
    let previous = found.delivered.clone();
    let on_update = found.on_update.clone();
    let live_for_task = live.clone();
    let next: Signal = async move {
        previous.await;
        let read = match &value_ref {
            None => Some(value.unwrap_or(Value::Null)),
            Some(payload_ref) => unpack_payload(payload_ref, &live_for_task.refs.reader, None)
                .await
                .ok(),
        };
        if let (Some(read), Some(on_update)) = (read, on_update) {
            on_update(read);
        }
        let _ = live_for_task
            .channel
            .send(&ToolChildMessage::Server(ServerToChild::UpdateAck {
                job_id,
                seq,
            }));
    }
    .boxed()
    .shared();
    found.delivered = next.clone();
    drop(pending);
    tokio::spawn(next);
}

fn on_exit(inner: &Arc<Inner>, live: &Arc<Live>) {
    live.supervisor.end();
    let failure = ToolChildError::new(
        "tool_child_exited",
        format!("generation {} exited", live.generation),
    );
    let jobs: Vec<Pending> = lock(&live.pending)
        .drain()
        .map(|(_, found)| found)
        .collect();
    for job in jobs {
        (job.finish)(Err(failure.clone()));
    }
    live.channel.close();
    live.containment.kill();
    {
        let mut state = lock(&inner.state);
        if state
            .live
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, live))
        {
            state.live = None;
            state.state = ToolChildState::Dead;
        }
    }
    if let Some(on_status) = &inner.options.on_status {
        on_status(ToolChildStatus {
            state: ToolChildStatusState::Exited,
            generation: live.generation,
            pid: live.pid.map_or(-1, i64::from),
        });
    }
    // The containment is closed on every exit, a failed teardown included.
    let ended: Signal = {
        let live = live.clone();
        async move {
            let _ = live.barrier.teardown(None).await;
            let _ = live.refs.writer.teardown().await;
            live.containment.close();
        }
        .boxed()
        .shared()
    };
    lock(&inner.state).last_ended = ended.clone();
    tokio::spawn(ended);
    if let Some(done) = lock(&live.exit_handled_tx).take() {
        let _ = done.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{connect_channel, open_channel_listener};

    // senpi#3566 todo 4: the tool child lifecycle. The upstream cases that need the `tool-child-fixture.ts` child (echo,
    // sleep, exit, spawnSleep, workerSpawn jobs) have no Rust counterpart here: the production child is this very
    // binary re-entered with the internal role (cli lane), so they live with that fixture. What needs no child runs
    // below, plus client-start.test.ts's start-failure and unsubscribe cases.

    const BOUND: Duration = Duration::from_secs(30);

    fn shell(script: &str) -> ToolChildCommand {
        ToolChildCommand {
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
        }
    }

    fn client_in(
        dir: PathBuf,
        cwd: PathBuf,
        command: ToolChildCommand,
        ready_ms: u64,
    ) -> ToolChildClient {
        ToolChildClient::new(ToolChildClientOptions {
            dir,
            cwd,
            command: Some(command),
            ready_ms: Some(ready_ms),
            ..ToolChildClientOptions::default()
        })
    }

    #[tokio::test]
    async fn rejects_a_job_when_no_child_is_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let instance = client_in(
            dir.path().join("ch"),
            dir.path().into(),
            shell("exit 0"),
            1_000,
        );
        let failure = instance
            .run("echo", Some(Value::from(1)), None, RunOptions::default())
            .await;
        assert_eq!(
            failure.expect_err("no child").code,
            "tool_child_unavailable"
        );
        assert_eq!(
            instance.ping().await.expect_err("no child").code,
            "tool_child_unavailable"
        );
    }

    #[tokio::test]
    async fn fails_a_start_whose_command_cannot_spawn_typed_and_is_not_left_spawning() {
        let dir = tempfile::tempdir().expect("tempdir");
        let command = ToolChildCommand {
            command: "/nonexistent/senpi".into(),
            args: Vec::new(),
        };
        let instance = client_in(dir.path().into(), dir.path().into(), command, 10_000);
        for _ in 0..2 {
            let failure = tokio::time::timeout(BOUND, instance.start())
                .await
                .expect("in time")
                .expect_err("no spawn");
            assert_eq!(failure.code, "tool_child_bootstrap_failed");
            assert_eq!(instance.state(), ToolChildState::Dead);
        }
    }

    #[tokio::test]
    async fn fails_a_start_whose_listener_cannot_open_typed_and_is_not_left_spawning() {
        let base = tempfile::tempdir().expect("tempdir");
        let file = base.path().join("not-a-dir");
        std::fs::write(&file, "").expect("write file");
        let instance = client_in(
            file.join("child"),
            base.path().into(),
            shell("exit 0"),
            10_000,
        );
        for _ in 0..2 {
            let failure = tokio::time::timeout(BOUND, instance.start())
                .await
                .expect("in time")
                .expect_err("no listener");
            assert_eq!(failure.code, "tool_child_bootstrap_failed");
            assert_eq!(instance.state(), ToolChildState::Dead);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_a_child_that_exits_before_ready_with_exited_before_ready() {
        let dir = tempfile::tempdir().expect("tempdir");
        let instance = client_in(
            dir.path().join("ch"),
            dir.path().into(),
            shell("exit 0"),
            20_000,
        );
        let failure = tokio::time::timeout(BOUND, instance.start())
            .await
            .expect("in time")
            .expect_err("exits");
        assert_eq!(failure.code, "tool_child_bootstrap_failed");
        assert!(
            failure.message.contains("exited_before_ready"),
            "{}",
            failure.message
        );
        assert_eq!(instance.state(), ToolChildState::Dead);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_a_child_that_never_says_hello_with_ready_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let instance = client_in(
            dir.path().join("ch"),
            dir.path().into(),
            shell("exec sleep 30"),
            1_500,
        );
        let failure = tokio::time::timeout(BOUND, instance.start())
            .await
            .expect("in time")
            .expect_err("hangs");
        assert_eq!(failure.code, "tool_child_bootstrap_failed");
        assert!(
            failure.message.contains("ready_timeout"),
            "{}",
            failure.message
        );
        assert_eq!(instance.state(), ToolChildState::Dead);
    }

    #[tokio::test]
    async fn stops_delivering_to_a_listener_once_it_unsubscribed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let listener = open_channel_listener(dir.path(), 1)
            .await
            .expect("listener");
        let client = connect_channel(&listener.env).await.expect("connect");
        let server = listener.accepted().await.expect("accepted");
        listener.close().await.expect("close listener");
        let heard: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
        let unsubscribe = {
            let heard = heard.clone();
            server.on_message(move |message| lock(&heard).push(message.message_type()))
        };
        let next_frame = |server: &ToolChildChannel| {
            let (tx, rx) = oneshot::channel();
            let tx = Arc::new(Mutex::new(Some(tx)));
            type Unsubscribe = Box<dyn FnOnce() + Send>;
            let stop: Arc<Mutex<Option<Unsubscribe>>> = Arc::new(Mutex::new(None));
            let stop_for_listener = stop.clone();
            let off = server.on_message(move |_| {
                if let Some(tx) = lock(&tx).take() {
                    let _ = tx.send(());
                }
                if let Some(off) = lock(&stop_for_listener).take() {
                    off();
                }
            });
            *lock(&stop) = Some(off);
            rx
        };
        let first = next_frame(&server);
        client
            .send(&ToolChildMessage::Child(ChildToServer::Pong { seq: 1 }))
            .expect("send");
        tokio::time::timeout(BOUND, first)
            .await
            .expect("in time")
            .expect("heard");
        unsubscribe();
        let second = next_frame(&server);
        client
            .send(&ToolChildMessage::Child(ChildToServer::Pong { seq: 2 }))
            .expect("send");
        tokio::time::timeout(BOUND, second)
            .await
            .expect("in time")
            .expect("heard");
        assert_eq!(*lock(&heard), vec!["pong"]);
        client.close();
        server.close();
    }

    #[test]
    fn the_error_carries_its_code_in_the_message() {
        let error = ToolChildError::new("tool_child_exited", "generation 1 exited");
        assert_eq!(error.code, "tool_child_exited");
        assert_eq!(error.to_string(), "tool_child_exited: generation 1 exited");
        assert_eq!(error.name(), "ToolChildError");
    }
}
