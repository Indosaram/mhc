//! Port of senpi `packages/agent/src/execution/testing/hand-harness.ts`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::types::{BoxFuture, ContentBlock};
use serde_json::json;
use tokio::sync::Notify;

use crate::durable::store::{DurableStore, DurableStoreOptions, StoreOperation, StoreWritePoint};
use crate::durable::store_admission::create_admission_line;
use crate::durable::testing::manual_timers::{ManualTimers, create_manual_timers};
use crate::durable::testing::store_cases::claimed_store;
use crate::durable::testing::store_harness::StoreConformanceAdapter;
use crate::execution::ledger::{StoreExecutionLedgerOptions, create_execution_ledger};
use crate::execution::ledger_types::ExecutionEntry;
use crate::execution::remote_executor::{
    HandCall, HandScope, RemoteExecutorOptions, create_remote_executor,
};
use crate::execution::remote_hand_ledger::{HandLedgerOptions, create_hand_ledger};
use crate::execution::remote_tool_host::{
    HandPlacement, RemoteToolHost, RemoteToolHostOptions, create_remote_tool_host,
};
use crate::execution::tool_host::{
    ExecutionScope, ToolHost, ToolHostCall, ToolHostOptions, ToolHostResult,
};
use crate::transport::in_process_carrier::{
    CarrierLink, InProcessNetworkOptions, create_in_process_network,
};
use crate::transport::local_endpoint::local_endpoint;
use crate::transport::testing::transport_harness::{
    TransportCaseAdapters, TransportConformanceAdapter,
};
use crate::transport::transport::{
    CarrierConnection, CarrierMessage, CarrierSide, TransportClock, TransportSide,
};
use crate::types::{AgentToolCall, AgentToolResult, ToolExecutionMode};

#[derive(Clone)]
pub struct HandCaseAdapters {
    pub store: Arc<dyn StoreConformanceAdapter>,
    pub transport: Arc<dyn TransportConformanceAdapter>,
    pub platform: String,
}

impl From<&HandCaseAdapters> for TransportCaseAdapters {
    fn from(value: &HandCaseAdapters) -> Self {
        Self {
            transport: Arc::clone(&value.transport),
            platform: value.platform.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillAt {
    Before,
    Durable,
}

impl KillAt {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::Durable => "durable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillPoint {
    pub commit: usize,
    pub at: KillAt,
}

pub fn call(id: &str) -> AgentToolCall {
    let mut arguments = serde_json::Map::new();
    arguments.insert("id".to_string(), json!(id));
    AgentToolCall {
        id: id.to_string(),
        name: "probe".to_string(),
        arguments,
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    }
}

const TICK_MS: i64 = 5;
const LEASE_MS: i64 = 3_600_000;

pub async fn drain_microtasks() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

#[derive(Clone)]
pub struct TransportPump {
    timers: ManualTimers,
}

impl TransportPump {
    pub fn new(timers: ManualTimers) -> Self {
        Self { timers }
    }

    pub fn timers(&self) -> &ManualTimers {
        &self.timers
    }

    async fn pump(&self, stop: impl Fn() -> bool, ms: i64) {
        let end = self.timers.now() + ms;
        drain_microtasks().await;
        while !stop() && self.timers.now() < end {
            let next = (self.timers.now() + TICK_MS).min(end);
            let _ = self.timers.advance_to(next).await;
            drain_microtasks().await;
        }
    }

    pub async fn advance(&self, ms: i64) {
        self.pump(|| false, ms).await;
    }

    pub async fn run_until(
        &self,
        done: impl Fn() -> bool,
        what: &str,
        budget_ms: Option<i64>,
    ) -> Result<(), String> {
        let budget = budget_ms.unwrap_or(600_000);
        self.pump(&done, budget).await;
        if !done() {
            return Err(format!(
                "{what}: not reached within {budget} ms of transport time"
            ));
        }
        Ok(())
    }

    pub async fn settle<T: Send + 'static>(
        &self,
        fut: impl Future<Output = T> + Send + 'static,
        what: &str,
        budget_ms: Option<i64>,
    ) -> Result<T, String> {
        let slot: Arc<Mutex<Option<T>>> = Arc::new(Mutex::new(None));
        let slot_c = Arc::clone(&slot);
        let done = Arc::new(AtomicBool::new(false));
        let done_c = Arc::clone(&done);
        tokio::spawn(async move {
            let val = fut.await;
            *slot_c.lock().unwrap_or_else(|p| p.into_inner()) = Some(val);
            done_c.store(true, Ordering::SeqCst);
        });
        self.run_until(|| done.load(Ordering::SeqCst), what, budget_ms)
            .await?;
        slot.lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .ok_or_else(|| format!("{what}: settled without value"))
    }

    pub async fn wait_for<F, Fut>(
        &self,
        mut done: F,
        what: &str,
        budget_ms: Option<i64>,
    ) -> Result<(), String>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<bool, String>>,
    {
        let budget = budget_ms.unwrap_or(600_000);
        let end = self.timers.now() + budget;
        drain_microtasks().await;
        while !done().await? {
            if self.timers.now() >= end {
                return Err(format!(
                    "{what}: not reached within {budget} ms of transport time"
                ));
            }
            let next = (self.timers.now() + TICK_MS).min(end);
            let _ = self.timers.advance_to(next).await;
            drain_microtasks().await;
        }
        Ok(())
    }
}

pub struct DirectCarrierSide(pub Arc<dyn TransportSide>);

impl CarrierSide for DirectCarrierSide {
    fn attached(&self, connection: Arc<dyn CarrierConnection>) {
        self.0.attached(connection);
    }

    fn message(&self, message: &CarrierMessage) {
        self.0.message(message);
    }

    fn writable(&self) {
        self.0.writable();
    }

    fn detached(&self) {
        self.0.detached();
    }
}

#[derive(Clone, Default)]
struct BodyGate {
    open: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl BodyGate {
    fn release(&self) {
        self.open.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.open.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

struct RigScope {
    scope_id: String,
    bodies: Arc<Mutex<Vec<String>>>,
    gated: Arc<AtomicBool>,
    gates: Arc<Mutex<HashMap<String, BodyGate>>>,
}

impl HandScope for RigScope {
    fn run(&self, hand_call: HandCall) -> BoxFuture<'_, Result<AgentToolResult, String>> {
        Box::pin(async move {
            self.bodies
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(format!("{}:{}", self.scope_id, hand_call.tool_call_id));
            if self.gated.load(Ordering::SeqCst) {
                let gate = {
                    let mut guard = self.gates.lock().unwrap_or_else(|p| p.into_inner());
                    guard
                        .entry(hand_call.tool_call_id.clone())
                        .or_default()
                        .clone()
                };
                gate.wait().await;
            }
            Ok(AgentToolResult {
                content: vec![ContentBlock::text(format!(
                    "ran:{}",
                    hand_call.tool_call_id
                ))],
                details: json!({}),
                structured_content: None,
                terminate: None,
                added_tool_names: None,
                usage: None,
                nested_calls: None,
                is_error: None,
            })
        })
    }

    fn is_sequential(&self, _name: &str) -> bool {
        false
    }

    fn dispose(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

pub struct HandRig {
    adapters: HandCaseAdapters,
    medium: String,
    timers: ManualTimers,
    pump: TransportPump,
    link: CarrierLink,
    hand_side: Arc<dyn TransportSide>,
    host: Arc<RemoteToolHost>,
    bodies: Arc<Mutex<Vec<String>>>,
    faults: Arc<Mutex<Vec<String>>>,
    gated: Arc<AtomicBool>,
    gates: Arc<Mutex<HashMap<String, BodyGate>>>,
    killed_flag: Arc<AtomicBool>,
    killed_notify: Arc<Notify>,
}

impl HandRig {
    pub fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn faults(&self) -> Vec<String> {
        self.faults
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub async fn wait_killed(&self) -> Result<(), String> {
        let flag = Arc::clone(&self.killed_flag);
        let notify = Arc::clone(&self.killed_notify);
        self.pump
            .settle(
                async move {
                    loop {
                        let notified = notify.notified();
                        if flag.load(Ordering::SeqCst) {
                            return;
                        }
                        notified.await;
                    }
                },
                "the kill point fired",
                None,
            )
            .await
    }

    pub fn execute(
        &self,
        scope: &str,
        calls: Vec<AgentToolCall>,
        mode: ToolExecutionMode,
    ) -> impl Future<Output = Result<ToolHostResult, String>> + Send + 'static {
        let host = Arc::clone(&self.host);
        let scope_id = scope.to_string();
        async move {
            host.execute(
                ToolHostCall {
                    scope: ExecutionScope {
                        id: scope_id.clone(),
                        root: format!("/work/{scope_id}"),
                    },
                    calls,
                    mode,
                },
                ToolHostOptions {
                    signal: None,
                    emit: Arc::new(|_| Box::pin(async {})),
                    record_witness: None,
                    local: None,
                },
            )
            .await
        }
    }

    pub async fn restart(&self) -> Result<(), String> {
        let store = self
            .adapters
            .store
            .open(&self.medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let exec = start_hand_executor(
            store,
            Arc::clone(&self.hand_side),
            self.timers.clone(),
            Arc::clone(&self.bodies),
            Arc::clone(&self.faults),
            Arc::clone(&self.gated),
            Arc::clone(&self.gates),
        );
        self.pump
            .settle(
                async move { exec.ready().await },
                "the restarted hand recovered",
                None,
            )
            .await
    }

    pub fn gate_bodies(&self) {
        self.gated.store(true, Ordering::SeqCst);
    }

    pub fn release(&self, tool_call_id: &str) {
        let gate = {
            let mut guard = self.gates.lock().unwrap_or_else(|p| p.into_inner());
            guard.entry(tool_call_id.to_string()).or_default().clone()
        };
        gate.release();
    }

    pub async fn advance(&self, ms: i64) {
        self.pump.advance(ms).await;
    }

    pub async fn settle<T: Send + 'static>(
        &self,
        fut: impl Future<Output = T> + Send + 'static,
        what: &str,
    ) -> Result<T, String> {
        self.pump.settle(fut, what, None).await
    }

    pub async fn run_until(&self, done: impl Fn() -> bool, what: &str) -> Result<(), String> {
        self.pump.run_until(done, what, None).await
    }

    pub async fn wait_for<F, Fut>(&self, done: F, what: &str) -> Result<(), String>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<bool, String>>,
    {
        self.pump.wait_for(done, what, None).await
    }

    pub fn disconnect(&self) {
        self.link.drop_link();
    }

    pub fn reconnect(&self) {
        self.link.connect();
    }

    pub async fn rows(&self) -> Result<Vec<ExecutionEntry>, String> {
        let store = self
            .adapters
            .store
            .open(&self.medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        create_execution_ledger(StoreExecutionLedgerOptions {
            store,
            epoch: 1,
            admission: create_admission_line(),
        })
        .scan()
        .await
        .map_err(|e| e.to_string())
    }
}

fn start_hand_executor(
    store: Arc<dyn DurableStore>,
    transport: Arc<dyn TransportSide>,
    timers: ManualTimers,
    bodies: Arc<Mutex<Vec<String>>>,
    faults: Arc<Mutex<Vec<String>>>,
    gated: Arc<AtomicBool>,
    gates: Arc<Mutex<HashMap<String, BodyGate>>>,
) -> Arc<crate::execution::remote_executor::RemoteExecutor> {
    let ledger = create_execution_ledger(StoreExecutionLedgerOptions {
        store,
        epoch: 1,
        admission: create_admission_line(),
    });
    create_remote_executor(RemoteExecutorOptions {
        authority: crate::execution::grant_verify::HandAuthority::SelfHost,
        transport,
        open_scope: Arc::new(move |scope| {
            let bodies = Arc::clone(&bodies);
            let gated = Arc::clone(&gated);
            let gates = Arc::clone(&gates);
            Box::pin(async move {
                Ok(Arc::new(RigScope {
                    scope_id: scope.id,
                    bodies,
                    gated,
                    gates,
                }) as Arc<dyn HandScope>)
            })
        }),
        ledger: Some(create_hand_ledger(HandLedgerOptions {
            ledger,
            executor: "hand".to_string(),
            issuer: "session".to_string(),
            workspace: "workspace".to_string(),
            now: Arc::new(move || timers.now()),
            lease_ms: LEASE_MS,
        })),
        fence: None,
        instance: None,
        file_channel: false,
        on_fault: Arc::new(move |err| {
            faults.lock().unwrap_or_else(|p| p.into_inner()).push(err);
        }),
    })
}

pub struct OpenHandRigOptions {
    pub placement: HandPlacement,
    pub kill_at: Option<KillPoint>,
    pub fault: Option<crate::transport::testing::fault_transport::FaultBuilder>,
}

pub async fn open_hand_rig(
    adapters: HandCaseAdapters,
    options: OpenHandRigOptions,
) -> Result<HandRig, String> {
    let timers = create_manual_timers(1_000);
    let timers_net = timers.clone();
    let network = create_in_process_network(InProcessNetworkOptions {
        schedule: Some(Arc::new(move |deliver| {
            let _ = timers_net.set_timer_sync(deliver, 0);
        })),
    });
    let clock: Arc<dyn TransportClock> = Arc::new(timers.clone());
    let session_side = adapters.transport.create_side(Arc::clone(&clock));
    let hand_side = adapters.transport.create_side(clock);
    let installed = options.fault.map(|build| build(&timers));
    let outer = |side: Arc<dyn TransportSide>| -> Arc<dyn CarrierSide> {
        let direct: Arc<dyn CarrierSide> = Arc::new(DirectCarrierSide(side));
        match &installed {
            Some(f) => f.wrap(direct),
            None => direct,
        }
    };
    let endpoint = local_endpoint(
        &adapters.platform,
        "hand-conformance",
        "/run/senpi-conformance",
    )?;
    let _ = network.listen(&endpoint, outer(Arc::clone(&hand_side)));
    let link = network.dial(&endpoint, outer(Arc::clone(&session_side)))?;
    link.connect();
    if let Some(f) = &installed {
        f.start(&link);
    }

    let (medium, _initial) = claimed_store(adapters.store.as_ref(), 1).await?;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let faults = Arc::new(Mutex::new(Vec::new()));
    let gated = Arc::new(AtomicBool::new(false));
    let gates = Arc::new(Mutex::new(HashMap::new()));
    let killed_flag = Arc::new(AtomicBool::new(false));
    let killed_notify = Arc::new(Notify::new());

    let commit_num = Arc::new(AtomicI64::new(-1));
    let dead = Arc::new(AtomicBool::new(false));
    let kill_opt = options.kill_at;
    let killed_flag_c = Arc::clone(&killed_flag);
    let killed_notify_c = Arc::clone(&killed_notify);
    let store = adapters
        .store
        .open(
            &medium,
            DurableStoreOptions {
                on_write_point: Some(Arc::new(move |point| {
                    if dead.load(Ordering::SeqCst) {
                        return Err(crate::durable::store::DurableStoreError::Other(
                            "the hand was killed".to_string(),
                        ));
                    }
                    let (at, is_commit) = match point {
                        StoreWritePoint::Before {
                            op: StoreOperation::Commit,
                        } => (KillAt::Before, true),
                        StoreWritePoint::Durable {
                            op: StoreOperation::Commit,
                        } => (KillAt::Durable, true),
                        _ => (KillAt::Before, false),
                    };
                    if !is_commit {
                        return Ok(());
                    }
                    let current = if at == KillAt::Before {
                        commit_num.fetch_add(1, Ordering::SeqCst) + 1
                    } else {
                        commit_num.load(Ordering::SeqCst)
                    };
                    if let Some(kill) = kill_opt
                        && current == kill.commit as i64
                        && kill.at == at
                    {
                        dead.store(true, Ordering::SeqCst);
                        killed_flag_c.store(true, Ordering::SeqCst);
                        killed_notify_c.notify_waiters();
                        return Err(crate::durable::store::DurableStoreError::Other(format!(
                            "hand killed {} commit {current}",
                            at.as_str()
                        )));
                    }
                    Ok(())
                })),
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    let exec = start_hand_executor(
        store,
        Arc::clone(&hand_side),
        timers.clone(),
        Arc::clone(&bodies),
        Arc::clone(&faults),
        Arc::clone(&gated),
        Arc::clone(&gates),
    );
    exec.ready().await;

    let host = create_remote_tool_host(RemoteToolHostOptions {
        transport: session_side,
        placement: options.placement,
        session_ref: None,
        mint_id: None,
        planes: None,
        file_planes: None,
        epoch: None,
        prompt_surface: None,
    });
    let pump = TransportPump::new(timers.clone());
    Ok(HandRig {
        adapters,
        medium,
        timers,
        pump,
        link,
        hand_side,
        host,
        bodies,
        faults,
        gated,
        gates,
        killed_flag,
        killed_notify,
    })
}
