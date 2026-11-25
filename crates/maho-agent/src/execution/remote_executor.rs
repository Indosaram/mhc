//! Port of senpi `packages/agent/src/execution/remote-executor.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::{AbortController, AbortSignal};
use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::transport::transport::{FrameHandler, Transport, TransportFrame};
use crate::types::AgentToolResult;

use super::device_files::{DeviceFileErrorCode, FILE_CHANNEL, RUN_CHANNEL};
use super::grant_verify::{GrantVerdict, HandAuthority, authorize};
use super::plane::{
    NestedRequestFn, PlaneClient, PlaneClientOptions, create_plane_client, decode_plane_message,
};
use super::remote_epoch::{HAND_SELF_SENDER, HandEpochFence, answer_floor};
use super::remote_file_call::{file_call, file_refusal, file_stopped};
use super::remote_hand_ledger::{HandAdmission, HandLedger};
use super::remote_protocol::{
    HAND_CONTROL_LANE, RemoteHandMessage, RemoteInvoke, decode_remote_message,
    encode_remote_message, to_remote_tool_result,
};
use super::tool_host::ExecutionScope;

#[derive(Clone)]
pub struct HandCall {
    pub tool_call_id: String,
    pub name: String,
    pub arguments: Value,
    pub signal: AbortSignal,
    pub nested: NestedRequestFn,
    pub prompt_surface: Option<String>,
}

pub trait HandScope: Send + Sync {
    fn run(&self, call: HandCall) -> BoxFuture<'_, Result<AgentToolResult, String>>;
    fn is_sequential(&self, name: &str) -> bool;
    fn file(
        &self,
        _op: &str,
        _args: &Value,
        _signal: AbortSignal,
        _nested: NestedRequestFn,
    ) -> Option<BoxFuture<'_, Result<AgentToolResult, String>>> {
        None
    }
    fn dispose(&self) -> BoxFuture<'_, Result<(), String>>;
}

pub type OpenHandScope = Arc<
    dyn Fn(ExecutionScope) -> BoxFuture<'static, Result<Arc<dyn HandScope>, String>> + Send + Sync,
>;

pub type ExecutorFaultFn = Arc<dyn Fn(String) + Send + Sync>;

pub struct RemoteExecutorOptions {
    pub transport: Arc<dyn Transport>,
    pub open_scope: OpenHandScope,
    pub authority: HandAuthority,
    pub ledger: Option<Arc<dyn HandLedger>>,
    pub fence: Option<Arc<HandEpochFence>>,
    pub instance: Option<String>,
    pub file_channel: bool,
    pub on_fault: ExecutorFaultFn,
}

#[derive(Clone)]
struct DoneWaiter {
    done: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl DoneWaiter {
    fn new() -> Self {
        Self {
            done: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.done.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

struct ScopeState {
    root: String,
    opened: Arc<tokio::sync::OnceCell<Result<Arc<dyn HandScope>, String>>>,
    last_sequential: Option<DoneWaiter>,
    wave: Vec<DoneWaiter>,
}

struct RunningCall {
    controller: AbortController,
    root: String,
}

struct ExecutorInner {
    transport: Arc<dyn Transport>,
    open_scope: OpenHandScope,
    authority: HandAuthority,
    sender: String,
    ledger: Option<Arc<dyn HandLedger>>,
    fence: Option<Arc<HandEpochFence>>,
    on_fault: ExecutorFaultFn,
    scopes: Mutex<HashMap<String, Arc<tokio::sync::Mutex<ScopeState>>>>,
    accepted: Mutex<Vec<String>>,
    seen: Mutex<HashSet<String>>,
    running: Mutex<HashMap<String, RunningCall>>,
    planes: Arc<PlaneClient>,
    closed: AtomicBool,
    ready_done: AtomicBool,
    ready_notify: Notify,
}

impl ExecutorInner {
    fn admits(&self, lane: &str, epoch: Option<u64>) -> bool {
        self.fence
            .as_ref()
            .is_none_or(|f| f.admits(lane, epoch, &self.sender))
    }

    fn reply(&self, lane: &str, message: &RemoteHandMessage) {
        let transport = Arc::clone(&self.transport);
        let lane = lane.to_owned();
        let bytes = encode_remote_message(message);
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = transport.send(&lane, bytes).await;
            });
        }
    }

    fn refuse(&self, lane: &str, execution_id: &str, reason: impl Into<String>) {
        self.reply(
            lane,
            &RemoteHandMessage::Refused {
                execution_id: execution_id.to_owned(),
                reason: reason.into(),
            },
        );
    }

    async fn get_or_open_scope(
        &self,
        scope: &ExecutionScope,
    ) -> Result<(Arc<tokio::sync::Mutex<ScopeState>>, Arc<dyn HandScope>), String> {
        let state_arc = {
            let mut map = self.scopes.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(existing) = map.get(&scope.id) {
                Arc::clone(existing)
            } else {
                let created = Arc::new(tokio::sync::Mutex::new(ScopeState {
                    root: scope.root.clone(),
                    opened: Arc::new(tokio::sync::OnceCell::new()),
                    last_sequential: None,
                    wave: Vec::new(),
                }));
                map.insert(scope.id.clone(), Arc::clone(&created));
                created
            }
        };

        let opened_cell = {
            let guard = state_arc.lock().await;
            if guard.root != scope.root {
                return Err("scope_binding_mismatch".to_owned());
            }
            Arc::clone(&guard.opened)
        };

        let scope_cloned = scope.clone();
        let open_res = opened_cell
            .get_or_init(|| async move { (self.open_scope)(scope_cloned).await })
            .await
            .clone();
        match open_res {
            Ok(hand_scope) => Ok((state_arc, hand_scope)),
            Err(reason) => Err(format!("scope_unavailable: {reason}")),
        }
    }

    async fn handle_invoke(
        self: &Arc<Self>,
        lane: &str,
        call: RemoteInvoke,
        file_op: Option<super::device_files::DeviceFileOp>,
    ) {
        if self.closed.load(Ordering::SeqCst) {
            self.refuse(lane, &call.execution_id, "hand_closed");
            return;
        }
        if lane != call.scope.id {
            self.refuse(lane, &call.execution_id, "scope_lane_mismatch");
            return;
        }
        let verdict = authorize(&self.authority, &call.scope.root);
        let granted_root = match verdict {
            GrantVerdict::Ok { root } => root,
            GrantVerdict::Denied { reason } => {
                self.refuse(lane, &call.execution_id, reason.as_str());
                return;
            }
        };
        if !self.admits(lane, call.epoch) {
            self.refuse(lane, &call.execution_id, "epoch_fenced");
            return;
        }
        {
            let mut seen = self.seen.lock().unwrap_or_else(|p| p.into_inner());
            if self.ledger.is_none() && seen.contains(&call.execution_id) {
                self.refuse(lane, &call.execution_id, "duplicate_execution");
                return;
            }
            seen.insert(call.execution_id.clone());
        }

        let bound_scope = ExecutionScope {
            id: call.scope.id.clone(),
            root: granted_root.clone(),
        };
        let (scope_mutex, hand_scope) = match self.get_or_open_scope(&bound_scope).await {
            Ok(pair) => pair,
            Err(reason) => {
                self.refuse(lane, &call.execution_id, reason);
                return;
            }
        };

        if let Some(ledger) = &self.ledger {
            let inner_admit = Arc::clone(self);
            let lane_admit = lane.to_owned();
            let epoch_admit = call.epoch;
            let admit_res = ledger
                .admit(
                    call.clone(),
                    Some(Arc::new(move || {
                        inner_admit.admits(&lane_admit, epoch_admit)
                    })),
                )
                .await;
            match admit_res {
                Ok(HandAdmission::Reply { message }) => {
                    self.reply(lane, &message);
                    return;
                }
                Ok(HandAdmission::InFlight) => return,
                Ok(HandAdmission::Run) => {}
                Err(err) => {
                    (self.on_fault)(err);
                    return;
                }
            }
        }

        self.accepted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(call.execution_id.clone());

        let controller = AbortController::new();
        self.running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                call.execution_id.clone(),
                RunningCall {
                    controller: controller.clone(),
                    root: granted_root,
                },
            );

        let sequential = file_op.is_some() || hand_scope.is_sequential(&call.name);
        let waiter = DoneWaiter::new();
        let dependencies: Vec<DoneWaiter> = {
            let mut guard = scope_mutex.lock().await;
            let mut deps = Vec::new();
            if let Some(last) = &guard.last_sequential {
                deps.push(last.clone());
            }
            if sequential {
                deps.append(&mut guard.wave);
                guard.last_sequential = Some(waiter.clone());
            } else {
                guard.wave.push(waiter.clone());
            }
            deps
        };

        let inner = Arc::clone(self);
        let lane_owned = lane.to_owned();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                struct WaiterGuard(DoneWaiter);
                impl Drop for WaiterGuard {
                    fn drop(&mut self) {
                        self.0.finish();
                    }
                }
                let _waiter_guard = WaiterGuard(waiter);

                for dep in dependencies {
                    dep.wait().await;
                }

                if inner.ledger.is_none() && !inner.admits(&lane_owned, call.epoch) {
                    inner
                        .running
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&call.execution_id);
                    inner.refuse(&lane_owned, &call.execution_id, "epoch_fenced");
                    return;
                }

                let signal = controller.signal();
                let nested =
                    inner
                        .planes
                        .request_for(&call.execution_id, &lane_owned, signal.clone());
                let granted = matches!(
                    authorize(&inner.authority, &call.scope.root),
                    GrantVerdict::Ok { .. }
                );
                let mut result = if signal.aborted() || !granted {
                    if let Some(_op) = file_op {
                        file_stopped(granted)
                    } else {
                        AgentToolResult {
                            content: vec![maho_ai::types::ContentBlock::text(
                                "The call's grant was revoked before it ran",
                            )],
                            details: Value::Object(Default::default()),
                            structured_content: None,
                            usage: None,
                            nested_calls: None,
                            added_tool_names: None,
                            terminate: None,
                            is_error: Some(true),
                        }
                    }
                } else {
                    match file_op {
                        None => hand_scope
                            .run(HandCall {
                                tool_call_id: call.tool_call_id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                                signal: signal.clone(),
                                nested,
                                prompt_surface: call.prompt_surface.clone(),
                            })
                            .await
                            .unwrap_or_else(|err| AgentToolResult {
                                content: vec![maho_ai::types::ContentBlock::text(err)],
                                details: Value::Object(Default::default()),
                                structured_content: None,
                                usage: None,
                                nested_calls: None,
                                added_tool_names: None,
                                terminate: None,
                                is_error: Some(true),
                            }),
                        Some(op) => match hand_scope.file(
                            op.as_str(),
                            &call.arguments,
                            signal.clone(),
                            nested,
                        ) {
                            None => file_refusal(
                                DeviceFileErrorCode::Unsupported,
                                "this hand serves no file channel",
                            ),
                            Some(fut) => match fut.await {
                                Ok(res) => res,
                                Err(err) => {
                                    let granted_now = matches!(
                                        authorize(&inner.authority, &call.scope.root),
                                        GrantVerdict::Ok { .. }
                                    );
                                    if signal.aborted() || !granted_now {
                                        file_stopped(granted_now)
                                    } else {
                                        file_refusal(DeviceFileErrorCode::Io, err)
                                    }
                                }
                            },
                        },
                    }
                };

                if file_op.is_some() && signal.aborted() {
                    let granted_now = matches!(
                        authorize(&inner.authority, &call.scope.root),
                        GrantVerdict::Ok { .. }
                    );
                    result = file_stopped(granted_now);
                }

                inner
                    .running
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&call.execution_id);
                inner.planes.parent_ended(&call.execution_id);

                let settled = to_remote_tool_result(&result);
                let out_msg = match &inner.ledger {
                    None => RemoteHandMessage::Settled {
                        execution_id: call.execution_id,
                        result: settled,
                    },
                    Some(ledger) => match ledger.settle(call, settled).await {
                        Ok(msg) => msg,
                        Err(err) => {
                            (inner.on_fault)(err);
                            return;
                        }
                    },
                };
                inner.reply(&lane_owned, &out_msg);
            });
        }
    }
}

pub struct RemoteExecutor {
    inner: Arc<ExecutorInner>,
    _stop_watching: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl RemoteExecutor {
    pub fn accepted(&self) -> Vec<String> {
        self.inner
            .accepted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub async fn ready(&self) {
        loop {
            let notified = self.inner.ready_notify.notified();
            if self.inner.ready_done.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub async fn close(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let controllers: Vec<AbortController> = self
            .inner
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .map(|r| r.controller.clone())
            .collect();
        for c in controllers {
            c.abort(None);
        }
        let scope_arcs: Vec<Arc<tokio::sync::Mutex<ScopeState>>> = self
            .inner
            .scopes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .map(|(_, s)| s)
            .collect();
        for scope_arc in scope_arcs {
            let guard = scope_arc.lock().await;
            if let Some(Ok(hand_scope)) = guard.opened.get() {
                let _ = hand_scope.dispose().await;
            }
        }
        self.inner.transport.close();
    }
}

pub fn create_remote_executor(options: RemoteExecutorOptions) -> Arc<RemoteExecutor> {
    let sender = match &options.authority {
        HandAuthority::Peer { peer, .. } => peer.clone(),
        HandAuthority::SelfHost => HAND_SELF_SENDER.to_owned(),
    };
    let planes = create_plane_client(PlaneClientOptions {
        transport: Arc::clone(&options.transport),
        mint_id: None,
    });
    let inner = Arc::new(ExecutorInner {
        transport: Arc::clone(&options.transport),
        open_scope: options.open_scope,
        authority: options.authority.clone(),
        sender,
        ledger: options.ledger,
        fence: options.fence,
        on_fault: options.on_fault,
        scopes: Mutex::new(HashMap::new()),
        accepted: Mutex::new(Vec::new()),
        seen: Mutex::new(HashSet::new()),
        running: Mutex::new(HashMap::new()),
        planes,
        closed: AtomicBool::new(false),
        ready_done: AtomicBool::new(false),
        ready_notify: Notify::new(),
    });

    let stop_watching = match &inner.authority {
        HandAuthority::Peer { peer, verifier } => {
            let inner_watch = Arc::clone(&inner);
            let peer = peer.clone();
            let verifier = Arc::clone(verifier);
            let verifier_cb = Arc::clone(&verifier);
            Some(verifier.on_replace(Arc::new(move || {
                let running: Vec<(AbortController, String)> = inner_watch
                    .running
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .values()
                    .map(|r| (r.controller.clone(), r.root.clone()))
                    .collect();
                for (controller, root) in running {
                    if !matches!(verifier_cb.verify(&peer, &root), GrantVerdict::Ok { .. }) {
                        controller.abort(None);
                    }
                }
            })))
        }
        HandAuthority::SelfHost => None,
    };

    let instance = options
        .instance
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let file_channel = options.file_channel;
    let inner_ready = Arc::clone(&inner);
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        rt.spawn(async move {
            let init_res: Result<(), String> = async {
                if let Some(fence) = &inner_ready.fence {
                    fence.ready().await?;
                }
                if let Some(ledger) = &inner_ready.ledger {
                    for recovered in ledger.recover().await? {
                        inner_ready.reply(&recovered.lane, &recovered.message);
                    }
                }
                let ops = if file_channel {
                    vec![FILE_CHANNEL.to_owned(), RUN_CHANNEL.to_owned()]
                } else {
                    Vec::new()
                };
                inner_ready.reply(
                    HAND_CONTROL_LANE,
                    &RemoteHandMessage::Hello {
                        instance,
                        durable: inner_ready.ledger.is_some(),
                        ops,
                    },
                );
                Ok(())
            }
            .await;
            if let Err(err) = init_res {
                (inner_ready.on_fault)(err);
            }
            inner_ready.ready_done.store(true, Ordering::SeqCst);
            inner_ready.ready_notify.notify_waiters();
        });
    } else {
        inner.ready_done.store(true, Ordering::SeqCst);
        inner.ready_notify.notify_waiters();
    }

    let inner_frame = Arc::clone(&inner);
    let handler: FrameHandler = Arc::new(move |frame: TransportFrame| {
        let inner = Arc::clone(&inner_frame);
        Box::pin(async move {
            loop {
                let notified = inner.ready_notify.notified();
                if inner.ready_done.load(Ordering::SeqCst) {
                    break;
                }
                notified.await;
            }
            if let Some(nested) = decode_plane_message(&frame.payload) {
                inner.planes.receive(nested);
                return;
            }
            let Ok(message) = decode_remote_message(&frame.payload) else {
                return;
            };
            match message {
                RemoteHandMessage::Invoke(invoke) => {
                    inner.handle_invoke(&frame.lane, invoke, None).await;
                }
                RemoteHandMessage::Floor { epoch } => {
                    match answer_floor(inner.fence.as_ref(), &frame.lane, epoch, &inner.sender)
                        .await
                    {
                        Ok(msg) => inner.reply(&frame.lane, &msg),
                        Err(err) => (inner.on_fault)(err),
                    }
                }
                RemoteHandMessage::Abort {
                    execution_id,
                    epoch,
                } => {
                    if !inner.admits(&frame.lane, epoch) {
                        return;
                    }
                    let ctrl = inner
                        .running
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get(&execution_id)
                        .map(|r| r.controller.clone());
                    if let Some(ctrl) = ctrl {
                        ctrl.abort(None);
                    }
                }
                RemoteHandMessage::Ack {
                    execution_id,
                    epoch,
                } => {
                    if !inner.admits(&frame.lane, epoch) {
                        return;
                    }
                    if let Some(ledger) = &inner.ledger {
                        let inner_admit = Arc::clone(&inner);
                        let lane = frame.lane.clone();
                        if let Err(err) = ledger
                            .ack(
                                &execution_id,
                                Some(Arc::new(move || inner_admit.admits(&lane, epoch))),
                            )
                            .await
                        {
                            (inner.on_fault)(err);
                        }
                    }
                }
                RemoteHandMessage::File(file_msg) => {
                    let op = file_msg.op;
                    let invoke = file_call(&file_msg);
                    inner.handle_invoke(&frame.lane, invoke, Some(op)).await;
                }
                _ => {}
            }
        })
    });

    inner.transport.on_frame(handler);
    Arc::new(RemoteExecutor {
        inner,
        _stop_watching: stop_watching,
    })
}
