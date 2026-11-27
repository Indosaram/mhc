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
    opened: Arc<tokio::sync::OnceCell<Arc<dyn HandScope>>>,
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
    active: Arc<tokio::sync::RwLock<()>>,
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
            let on_fault = Arc::clone(&self.on_fault);
            rt.spawn(async move {
                if let Err(err) = transport.send(&lane, bytes).await {
                    on_fault(format!("failed to send on lane {lane}: {err}"));
                }
            });
        } else {
            (self.on_fault)(format!("cannot send on lane {lane}: no runtime"));
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
            .get_or_try_init(|| async move { (self.open_scope)(scope_cloned).await })
            .await;
        match open_res {
            Ok(hand_scope) => Ok((state_arc, Arc::clone(hand_scope))),
            Err(reason) => Err(format!("scope_unavailable: {reason}")),
        }
    }

    async fn handle_invoke(
        self: &Arc<Self>,
        lane: &str,
        call: RemoteInvoke,
        file_op: Option<super::device_files::DeviceFileOp>,
    ) {
        let active = Arc::clone(&self.active).read_owned().await;
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
                if self.ledger.is_none() {
                    self.seen
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&call.execution_id);
                }
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

        let controller = AbortController::new();
        let registered = {
            let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner());
            if self.closed.load(Ordering::SeqCst) {
                false
            } else {
                running.insert(
                    call.execution_id.clone(),
                    RunningCall {
                        controller: controller.clone(),
                        root: granted_root,
                    },
                );
                true
            }
        };
        if !registered {
            self.refuse(lane, &call.execution_id, "hand_closed");
            return;
        }
        self.accepted
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(call.execution_id.clone());

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
                let _active = active;
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
        let active = self.inner.active.write().await;
        drop(active);
        let scope_arcs: Vec<Arc<tokio::sync::Mutex<ScopeState>>> = self
            .inner
            .scopes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .map(|(_, s)| s)
            .collect();
        for scope_arc in scope_arcs {
            let (waiters, hand_scope) = {
                let guard = scope_arc.lock().await;
                let mut waiters = guard.wave.clone();
                if let Some(last) = &guard.last_sequential {
                    waiters.push(last.clone());
                }
                (waiters, guard.opened.get().cloned())
            };
            for waiter in waiters {
                waiter.wait().await;
            }
            if let Some(hand_scope) = hand_scope
                && let Err(err) = hand_scope.dispose().await
            {
                (self.inner.on_fault)(format!("scope disposal failed: {err}"));
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
        active: Arc::new(tokio::sync::RwLock::new(())),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::transport::TransportClosedError;

    struct ClosedTransport;

    impl Transport for ClosedTransport {
        fn send(&self, _: &str, _: Vec<u8>) -> BoxFuture<'_, Result<(), TransportClosedError>> {
            Box::pin(async { Err(TransportClosedError) })
        }
        fn on_frame(&self, _: FrameHandler) {}
        fn in_flight(&self, _: &str) -> usize {
            0
        }
        fn close(&self) {}
    }

    struct OpenTransport;
    impl Transport for OpenTransport {
        fn send(&self, _: &str, _: Vec<u8>) -> BoxFuture<'_, Result<(), TransportClosedError>> {
            Box::pin(async { Ok(()) })
        }
        fn on_frame(&self, _: FrameHandler) {}
        fn in_flight(&self, _: &str) -> usize {
            0
        }
        fn close(&self) {}
    }

    fn executor(fault: ExecutorFaultFn) -> Arc<RemoteExecutor> {
        create_remote_executor(RemoteExecutorOptions {
            transport: Arc::new(ClosedTransport),
            open_scope: Arc::new(|_| Box::pin(async { Err("not used".to_owned()) })),
            authority: HandAuthority::SelfHost,
            ledger: None,
            fence: None,
            instance: Some("test".to_owned()),
            file_channel: false,
            on_fault: fault,
        })
    }

    #[tokio::test]
    async fn failed_hello_send_reports_fault() {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let sender = Mutex::new(Some(sender));
        let _executor = executor(Arc::new(move |fault| {
            if let Some(sender) = sender.lock().expect("fault sender").take() {
                let _ = sender.send(fault);
            }
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
            .await
            .expect("send failure is reported")
            .expect("fault callback is alive");
    }

    #[test]
    fn reply_without_runtime_reports_fault() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let executor = executor(Arc::new(move |fault| {
            sender.send(fault).expect("test receives fault");
        }));
        executor.inner.reply(
            "lane",
            &RemoteHandMessage::Hello {
                instance: "test".to_owned(),
                durable: false,
                ops: Vec::new(),
            },
        );
        assert!(receiver.try_recv().is_ok());
    }

    #[tokio::test]
    async fn close_waits_for_calls_and_disposes_without_scope_lock() {
        struct Scope {
            state: Mutex<std::sync::Weak<tokio::sync::Mutex<ScopeState>>>,
            done: DoneWaiter,
            observed: tokio::sync::mpsc::UnboundedSender<(bool, bool)>,
        }
        impl HandScope for Scope {
            fn run(&self, _: HandCall) -> BoxFuture<'_, Result<AgentToolResult, String>> {
                unreachable!("this test installs an active call directly")
            }
            fn is_sequential(&self, _: &str) -> bool {
                false
            }
            fn dispose(&self) -> BoxFuture<'_, Result<(), String>> {
                Box::pin(async move {
                    let state = self
                        .state
                        .lock()
                        .expect("scope state")
                        .upgrade()
                        .expect("alive");
                    let unlocked = state.try_lock().is_ok();
                    self.observed
                        .send((self.done.done.load(Ordering::SeqCst), unlocked))
                        .expect("observe disposal");
                    Err("dispose failed".to_owned())
                })
            }
        }
        let (fault_tx, mut fault_rx) = tokio::sync::mpsc::unbounded_channel();
        let executor = create_remote_executor(RemoteExecutorOptions {
            transport: Arc::new(OpenTransport),
            open_scope: Arc::new(|_| Box::pin(async { Err("not used".to_owned()) })),
            authority: HandAuthority::SelfHost,
            ledger: None,
            fence: None,
            instance: Some("test".to_owned()),
            file_channel: false,
            on_fault: Arc::new(move |fault| {
                fault_tx.send(fault).expect("observe fault");
            }),
        });
        let waiter = DoneWaiter::new();
        let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
        let scope = Arc::new(Scope {
            state: Mutex::new(std::sync::Weak::new()),
            done: waiter.clone(),
            observed: observed_tx,
        });
        let opened = Arc::new(tokio::sync::OnceCell::new());
        assert!(opened.set(scope.clone() as Arc<dyn HandScope>).is_ok());
        let state = Arc::new(tokio::sync::Mutex::new(ScopeState {
            root: "root".to_owned(),
            opened,
            last_sequential: None,
            wave: vec![waiter.clone()],
        }));
        *scope.state.lock().expect("scope state") = Arc::downgrade(&state);
        executor
            .inner
            .scopes
            .lock()
            .expect("scopes")
            .insert("lane".to_owned(), state);
        let controller = AbortController::new();
        let signal = controller.signal();
        executor.inner.running.lock().expect("running").insert(
            "call".to_owned(),
            RunningCall {
                controller,
                root: "root".to_owned(),
            },
        );
        let closing = tokio::spawn(async move { executor.close().await });
        tokio::time::timeout(std::time::Duration::from_secs(5), signal.cancelled())
            .await
            .expect("call is cancelled");
        waiter.finish();
        tokio::time::timeout(std::time::Duration::from_secs(5), closing)
            .await
            .expect("close completes")
            .expect("close task");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), observed_rx.recv())
                .await
                .expect("disposal observed"),
            Some((true, true))
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), fault_rx.recv())
                .await
                .expect("dispose fault reported")
                .expect("fault callback alive")
                .contains("dispose failed")
        );
    }

    #[tokio::test]
    async fn invocation_is_not_accepted_after_close_during_scope_open() {
        struct Scope;
        impl HandScope for Scope {
            fn run(&self, _: HandCall) -> BoxFuture<'_, Result<AgentToolResult, String>> {
                Box::pin(async { Err("unexpected run".to_owned()) })
            }
            fn is_sequential(&self, _: &str) -> bool {
                false
            }
            fn dispose(&self) -> BoxFuture<'_, Result<(), String>> {
                Box::pin(async { Ok(()) })
            }
        }
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
        let release = Arc::new(Notify::new());
        let open_release = release.clone();
        let executor = create_remote_executor(RemoteExecutorOptions {
            transport: Arc::new(OpenTransport),
            open_scope: Arc::new(move |_| {
                let release = open_release.clone();
                let entered = entered_tx.lock().expect("open signal").take();
                Box::pin(async move {
                    entered.expect("first open").send(()).expect("observe open");
                    release.notified().await;
                    Ok(Arc::new(Scope) as Arc<dyn HandScope>)
                })
            }),
            authority: HandAuthority::SelfHost,
            ledger: None,
            fence: None,
            instance: Some("test".to_owned()),
            file_channel: false,
            on_fault: Arc::new(|fault| panic!("unexpected fault: {fault}")),
        });
        let inner = executor.inner.clone();
        let invocation = tokio::spawn(async move {
            inner
                .handle_invoke(
                    "lane",
                    RemoteInvoke {
                        epoch: None,
                        execution_id: "execution".to_owned(),
                        batch_id: "batch".to_owned(),
                        index: 0,
                        scope: ExecutionScope {
                            id: "lane".to_owned(),
                            root: "root".to_owned(),
                        },
                        tool_call_id: "call".to_owned(),
                        name: "tool".to_owned(),
                        arguments: Value::Null,
                        session_ref: None,
                        prompt_surface: None,
                    },
                    None,
                )
                .await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), entered_rx)
            .await
            .expect("scope open started")
            .expect("open callback alive");
        let mut close = Box::pin(executor.close());
        assert!(matches!(
            futures::poll!(close.as_mut()),
            std::task::Poll::Pending
        ));
        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), invocation)
            .await
            .expect("invocation completes")
            .expect("invocation task");
        assert!(executor.accepted().is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(5), close)
            .await
            .expect("close waits for scope opening");
    }

    #[tokio::test]
    async fn failed_scope_open_allows_same_and_new_execution_retries() {
        struct Scope;
        impl HandScope for Scope {
            fn run(&self, _: HandCall) -> BoxFuture<'_, Result<AgentToolResult, String>> {
                Box::pin(async { Err("test result".to_owned()) })
            }
            fn is_sequential(&self, _: &str) -> bool {
                false
            }
            fn dispose(&self) -> BoxFuture<'_, Result<(), String>> {
                Box::pin(async { Ok(()) })
            }
        }
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let open_attempts = attempts.clone();
        let executor = create_remote_executor(RemoteExecutorOptions {
            transport: Arc::new(OpenTransport),
            open_scope: Arc::new(move |_| {
                let attempt = open_attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if attempt == 0 {
                        Err("temporary failure".to_owned())
                    } else {
                        Ok(Arc::new(Scope) as Arc<dyn HandScope>)
                    }
                })
            }),
            authority: HandAuthority::SelfHost,
            ledger: None,
            fence: None,
            instance: Some("test".to_owned()),
            file_channel: false,
            on_fault: Arc::new(|fault| panic!("unexpected fault: {fault}")),
        });
        let mut call = RemoteInvoke {
            epoch: None,
            execution_id: "first".to_owned(),
            batch_id: "batch".to_owned(),
            index: 0,
            scope: ExecutionScope {
                id: "lane".to_owned(),
                root: "root".to_owned(),
            },
            tool_call_id: "call".to_owned(),
            name: "tool".to_owned(),
            arguments: Value::Null,
            session_ref: None,
            prompt_surface: None,
        };
        executor
            .inner
            .handle_invoke("lane", call.clone(), None)
            .await;
        call.execution_id = "second".to_owned();
        executor
            .inner
            .handle_invoke("lane", call.clone(), None)
            .await;
        call.execution_id = "first".to_owned();
        executor.inner.handle_invoke("lane", call, None).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(executor.accepted(), ["second", "first"]);
        executor.close().await;
    }
}
