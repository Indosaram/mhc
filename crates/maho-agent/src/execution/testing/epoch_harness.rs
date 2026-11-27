//! Port of senpi `packages/agent/src/execution/testing/epoch-harness.ts`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::types::{BoxFuture, ContentBlock};
use serde_json::json;
use tokio::sync::Notify;

use crate::durable::store::{
    AdoptCallback, CommitResult, DurableStore, DurableStoreError, DurableStoreOptions, RecordScan,
    StoreDocument, StoreRecord, StoreSnapshot, StoreValue, StoreWrite,
};
use crate::durable::store_admission::create_admission_line;
use crate::durable::testing::manual_timers::create_manual_timers;
use crate::durable::testing::store_cases::claimed_store;
use crate::execution::grant_verify::{
    GrantRow, GrantSnapshot, GrantVerifier, HandAuthority, WorkspaceRoot, create_grant_verifier,
};
use crate::execution::ledger::{StoreExecutionLedgerOptions, create_execution_ledger};
use crate::execution::ledger_types::ExecutionEntry;
use crate::execution::remote_epoch::{
    DurableEpochOptions, EPOCH_FLOOR_PREFIX, HandEpochFenceOptions, create_hand_epoch_fence,
};
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
use crate::transport::in_process_carrier::{InProcessNetworkOptions, create_in_process_network};
use crate::transport::local_endpoint::local_endpoint;
use crate::transport::transport::{CarrierSide, TransportClock, TransportSide};
use crate::types::{AgentToolCall, AgentToolResult, ToolExecutionMode};

use super::hand_harness::{DirectCarrierSide, HandCaseAdapters, KillAt, TransportPump};

pub type CommitMatch = Arc<dyn Fn(&[StoreWrite]) -> bool + Send + Sync>;

#[derive(Debug, Clone)]
pub struct EpochBrainSpec {
    pub name: String,
    pub epoch: u64,
}

const LEASE_MS: i64 = 3_600_000;
const HAND: &str = "hand";

fn field<'a>(write: &'a StoreWrite, name: &str) -> Option<&'a StoreValue> {
    let data = match write {
        StoreWrite::Put { data, .. } => data,
        StoreWrite::Append { data, .. } => data,
        StoreWrite::Delete { .. } => return None,
    };
    data.as_object()?.get(name)
}

pub fn floor_commit(scope: &str, epoch: u64) -> CommitMatch {
    let key = format!("{EPOCH_FLOOR_PREFIX}{scope}");
    Arc::new(move |writes| {
        writes.iter().any(|write| {
            matches!(write, StoreWrite::Put { key: k, .. } if k == &key)
                && field(write, "epoch").and_then(StoreValue::as_u64) == Some(epoch)
        })
    })
}

pub fn step_commit(brain: &str, event: &str) -> CommitMatch {
    let prefix = format!("{brain}-");
    let ev = event.to_string();
    Arc::new(move |writes| {
        writes.iter().any(|write| {
            matches!(write, StoreWrite::Append { .. })
                && field(write, "event").and_then(StoreValue::as_str) == Some(ev.as_str())
                && field(write, "commandId")
                    .and_then(StoreValue::as_str)
                    .is_some_and(|id| id.starts_with(&prefix))
        })
    })
}

#[derive(Clone)]
pub struct EpochHold {
    match_fn: CommitMatch,
    reached: Arc<AtomicBool>,
    reached_notify: Arc<Notify>,
    released: Arc<AtomicBool>,
    release_notify: Arc<Notify>,
}

pub struct EpochHoldHandle {
    hold: EpochHold,
}

impl EpochHoldHandle {
    pub async fn wait_reached(&self) {
        loop {
            let notified = self.hold.reached_notify.notified();
            if self.hold.reached.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    pub fn release(&self) {
        self.hold.released.store(true, Ordering::SeqCst);
        self.hold.release_notify.notify_waiters();
    }
}

#[derive(Clone)]
pub struct ProcessKill {
    pub match_fn: CommitMatch,
    pub at: KillAt,
    pub die: Arc<dyn Fn() + Send + Sync>,
}

struct ProcessStore {
    inner: Arc<dyn DurableStore>,
    holds: Arc<Mutex<Vec<EpochHold>>>,
    kill: Option<ProcessKill>,
    kill_armed: AtomicBool,
    dead: AtomicBool,
}

impl ProcessStore {
    fn alive(&self) -> Result<(), DurableStoreError> {
        if self.dead.load(Ordering::SeqCst) {
            Err(DurableStoreError::Other("the hand was killed".to_string()))
        } else {
            Ok(())
        }
    }

    fn die(&self, at: &str) -> DurableStoreError {
        self.dead.store(true, Ordering::SeqCst);
        if let Some(kill) = &self.kill {
            (kill.die)();
        }
        DurableStoreError::Other(format!("hand killed {at} the matched commit"))
    }
}

impl DurableStore for ProcessStore {
    fn read_epoch(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
        Box::pin(async move {
            self.alive()?;
            self.inner.read_epoch().await
        })
    }

    fn read_head(&self) -> BoxFuture<'_, Result<u64, DurableStoreError>> {
        Box::pin(async move {
            self.alive()?;
            self.inner.read_head().await
        })
    }

    fn claim(&self, expected: u64, next: u64) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        Box::pin(async move {
            self.alive()?;
            self.inner.claim(expected, next).await
        })
    }

    fn mint_id(&self) -> String {
        self.inner.mint_id()
    }

    fn scan_records(
        &self,
        query: RecordScan,
    ) -> BoxFuture<'_, Result<Vec<StoreRecord>, DurableStoreError>> {
        Box::pin(async move {
            self.alive()?;
            self.inner.scan_records(query).await
        })
    }

    fn get_document(
        &self,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<StoreValue>, DurableStoreError>> {
        let key = key.to_owned();
        Box::pin(async move {
            self.alive()?;
            self.inner.get_document(&key).await
        })
    }

    fn scan_documents(
        &self,
        prefix: &str,
    ) -> BoxFuture<'_, Result<Vec<StoreDocument>, DurableStoreError>> {
        let prefix = prefix.to_owned();
        Box::pin(async move {
            self.alive()?;
            self.inner.scan_documents(&prefix).await
        })
    }

    fn export_snapshot(&self) -> BoxFuture<'_, Result<StoreSnapshot, DurableStoreError>> {
        Box::pin(async move {
            self.alive()?;
            self.inner.export_snapshot().await
        })
    }

    fn import_snapshot(
        &self,
        epoch: u64,
        snapshot: StoreSnapshot,
    ) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        Box::pin(async move {
            self.alive()?;
            self.inner.import_snapshot(epoch, snapshot).await
        })
    }

    fn commit<'a>(
        &'a self,
        epoch: u64,
        writes: Vec<StoreWrite>,
        adopt: AdoptCallback<'a>,
    ) -> BoxFuture<'a, Result<CommitResult, DurableStoreError>> {
        Box::pin(async move {
            self.alive()?;
            let matched_hold = {
                let mut guard = self.holds.lock().unwrap_or_else(|p| p.into_inner());
                guard
                    .iter()
                    .position(|h| (h.match_fn)(&writes))
                    .map(|idx| guard.remove(idx))
            };
            if let Some(hold) = matched_hold {
                hold.reached.store(true, Ordering::SeqCst);
                hold.reached_notify.notify_waiters();
                loop {
                    let notified = hold.release_notify.notified();
                    if hold.released.load(Ordering::SeqCst) {
                        break;
                    }
                    notified.await;
                }
                self.alive()?;
            }
            let should_kill = self.kill_armed.load(Ordering::SeqCst)
                && self.kill.as_ref().is_some_and(|k| (k.match_fn)(&writes));
            if !should_kill {
                return self.inner.commit(epoch, writes, adopt).await;
            }
            self.kill_armed.store(false, Ordering::SeqCst);
            let at = self.kill.as_ref().map_or(KillAt::Before, |k| k.at);
            if at == KillAt::Before {
                return Err(self.die("before"));
            }
            let _ = self.inner.commit(epoch, writes, Box::new(|_| {})).await?;
            Err(self.die("once durable"))
        })
    }
}

pub struct EpochRig {
    adapters: HandCaseAdapters,
    medium: String,
    brains: Vec<EpochBrainSpec>,
    verifier: Arc<crate::execution::grant_verify::DefaultGrantVerifier>,
    hand_sides: HashMap<String, Arc<dyn TransportSide>>,
    hosts: HashMap<String, Arc<RemoteToolHost>>,
    owners: Arc<Mutex<HashMap<String, String>>>,
    bodies: Arc<Mutex<Vec<String>>>,
    faults: Arc<Mutex<Vec<String>>>,
    holds: Arc<Mutex<Vec<EpochHold>>>,
    killed_flag: Arc<AtomicBool>,
    killed_notify: Arc<Notify>,
    pump: TransportPump,
}

impl EpochRig {
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

    pub fn execute(
        &self,
        brain: &str,
        scope: &str,
        calls: Vec<AgentToolCall>,
    ) -> impl Future<Output = Result<ToolHostResult, String>> + Send + 'static {
        let host = self.hosts.get(brain).cloned();
        let brain = brain.to_string();
        let scope_id = scope.to_string();
        async move {
            let Some(host) = host else {
                return Err(format!("no brain {brain}"));
            };
            host.execute(
                ToolHostCall {
                    scope: ExecutionScope {
                        id: scope_id.clone(),
                        root: format!("/work/{scope_id}"),
                    },
                    calls,
                    mode: ToolExecutionMode::Parallel,
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

    pub fn set_owner(&self, scope: &str, brain: &str) {
        self.owners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(scope.to_string(), brain.to_string());
    }

    pub fn hold(&self, match_fn: CommitMatch) -> EpochHoldHandle {
        let hold = EpochHold {
            match_fn,
            reached: Arc::new(AtomicBool::new(false)),
            reached_notify: Arc::new(Notify::new()),
            released: Arc::new(AtomicBool::new(false)),
            release_notify: Arc::new(Notify::new()),
        };
        self.holds
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(hold.clone());
        EpochHoldHandle { hold }
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

    pub async fn restart(&self) -> Result<(), String> {
        let raw = self
            .adapters
            .store
            .open(&self.medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        let store: Arc<dyn DurableStore> = Arc::new(ProcessStore {
            inner: raw,
            holds: Arc::clone(&self.holds),
            kill: None,
            kill_armed: AtomicBool::new(false),
            dead: AtomicBool::new(false),
        });
        start_epoch_hand(
            store,
            &self.brains,
            &self.hand_sides,
            Arc::clone(&self.verifier),
            Arc::clone(&self.owners),
            Arc::clone(&self.bodies),
            Arc::clone(&self.faults),
            &self.pump,
        )
        .await
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

    pub async fn floor(&self, scope: &str) -> Result<Option<StoreValue>, String> {
        let store = self
            .adapters
            .store
            .open(&self.medium, DurableStoreOptions::default())
            .await
            .map_err(|e| e.to_string())?;
        store
            .get_document(&format!("{EPOCH_FLOOR_PREFIX}{scope}"))
            .await
            .map_err(|e| e.to_string())
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
}

#[allow(clippy::too_many_arguments)]
async fn start_epoch_hand(
    store: Arc<dyn DurableStore>,
    brains: &[EpochBrainSpec],
    hand_sides: &HashMap<String, Arc<dyn TransportSide>>,
    verifier: Arc<crate::execution::grant_verify::DefaultGrantVerifier>,
    owners: Arc<Mutex<HashMap<String, String>>>,
    bodies: Arc<Mutex<Vec<String>>>,
    faults: Arc<Mutex<Vec<String>>>,
    pump: &TransportPump,
) -> Result<(), String> {
    let admission = create_admission_line();
    let ledger = create_execution_ledger(StoreExecutionLedgerOptions {
        store: Arc::clone(&store),
        epoch: 1,
        admission: admission.clone(),
    });
    let now_timers = pump.timers().clone();
    let hand = create_hand_ledger(HandLedgerOptions {
        ledger,
        executor: HAND.to_string(),
        issuer: "session".to_string(),
        workspace: "workspace".to_string(),
        now: Arc::new(move || now_timers.now()),
        lease_ms: LEASE_MS,
    });
    let fence = create_hand_epoch_fence(HandEpochFenceOptions {
        owner_of: Arc::new(move |scope| {
            owners
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(scope)
                .cloned()
        }),
        durable: Some(DurableEpochOptions {
            store,
            epoch: 1,
            admission,
        }),
    });
    let mut executors = Vec::with_capacity(brains.len());
    for spec in brains {
        let transport = hand_sides
            .get(&spec.name)
            .cloned()
            .ok_or_else(|| format!("no link for {}", spec.name))?;
        let bodies_c = Arc::clone(&bodies);
        let faults_c = Arc::clone(&faults);
        let verifier_dyn: Arc<dyn GrantVerifier> = verifier.clone();
        executors.push(create_remote_executor(RemoteExecutorOptions {
            authority: HandAuthority::Peer {
                peer: spec.name.clone(),
                verifier: verifier_dyn,
            },
            transport,
            open_scope: Arc::new(move |scope| {
                let bodies_inner = Arc::clone(&bodies_c);
                Box::pin(async move {
                    struct EpochScope {
                        scope_id: String,
                        bodies: Arc<Mutex<Vec<String>>>,
                    }
                    impl HandScope for EpochScope {
                        fn run(
                            &self,
                            hand_call: HandCall,
                        ) -> BoxFuture<'_, Result<AgentToolResult, String>>
                        {
                            Box::pin(async move {
                                self.bodies
                                    .lock()
                                    .unwrap_or_else(|p| p.into_inner())
                                    .push(format!("{}:{}", self.scope_id, hand_call.tool_call_id));
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
                    Ok(Arc::new(EpochScope {
                        scope_id: scope.id,
                        bodies: bodies_inner,
                    }) as Arc<dyn HandScope>)
                })
            }),
            ledger: Some(Arc::clone(&hand)),
            fence: Some(Arc::clone(&fence)),
            instance: None,
            file_channel: false,
            on_fault: Arc::new(move |err| {
                faults_c.lock().unwrap_or_else(|p| p.into_inner()).push(err);
            }),
        }));
    }
    pump.settle(
        async move {
            for exec in executors {
                exec.ready().await;
            }
        },
        "every link's executor is ready",
        None,
    )
    .await
}

pub struct OpenEpochRigOptions {
    pub brains: Vec<EpochBrainSpec>,
    pub placement: Option<HandPlacement>,
    pub kill: Option<(CommitMatch, KillAt)>,
}

pub async fn open_epoch_rig(
    adapters: HandCaseAdapters,
    options: OpenEpochRigOptions,
) -> Result<EpochRig, String> {
    let timers = create_manual_timers(1_000);
    let pump = TransportPump::new(timers.clone());
    let timers_net = timers.clone();
    let network = create_in_process_network(InProcessNetworkOptions {
        schedule: Some(Arc::new(move |deliver| {
            let _ = timers_net.set_timer_sync(deliver, 0);
        })),
    });
    let verifier = create_grant_verifier(crate::execution::grant_verify::GrantVerifierOptions {
        self_id: HAND.to_string(),
        workspaces: vec![WorkspaceRoot {
            root: "/work".to_string(),
            label: None,
        }],
        canonicalize: Arc::new(|path| Ok(path.to_string())),
        now: None,
    });
    verifier.install(GrantSnapshot {
        rv: 1,
        expires_at: i64::MAX,
        grants: options
            .brains
            .iter()
            .map(|b| GrantRow {
                holder: b.name.clone(),
                target: HAND.to_string(),
                workspace: "omo".to_string(),
                capability: "execute".to_string(),
            })
            .collect(),
    })?;

    let mut hand_sides = HashMap::new();
    let mut hosts = HashMap::new();
    let clock: Arc<dyn TransportClock> = Arc::new(timers);
    let placement = options.placement.unwrap_or(HandPlacement::CrossMachine);
    for brain in &options.brains {
        let session_side = adapters.transport.create_side(Arc::clone(&clock));
        let hand_side = adapters.transport.create_side(Arc::clone(&clock));
        let endpoint = local_endpoint(
            &adapters.platform,
            &format!("epoch-{}", brain.name),
            "/run/senpi-conformance",
        )?;
        let _ = network.listen(
            &endpoint,
            Arc::new(DirectCarrierSide(Arc::clone(&hand_side))) as Arc<dyn CarrierSide>,
        );
        network
            .dial(
                &endpoint,
                Arc::new(DirectCarrierSide(Arc::clone(&session_side))) as Arc<dyn CarrierSide>,
            )?
            .connect();
        hand_sides.insert(brain.name.clone(), hand_side);
        let minted = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let brain_name = brain.name.clone();
        hosts.insert(
            brain.name.clone(),
            create_remote_tool_host(RemoteToolHostOptions {
                transport: session_side,
                placement,
                session_ref: None,
                mint_id: Some(Arc::new(move || {
                    let n = minted.fetch_add(1, Ordering::SeqCst);
                    format!("{brain_name}-{n}")
                })),
                planes: None,
                file_planes: None,
                epoch: Some(brain.epoch),
                prompt_surface: None,
            }),
        );
    }

    let (medium, _initial) = claimed_store(adapters.store.as_ref(), 1).await?;
    let owners = Arc::new(Mutex::new(HashMap::new()));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let faults = Arc::new(Mutex::new(Vec::new()));
    let holds = Arc::new(Mutex::new(Vec::new()));
    let killed_flag = Arc::new(AtomicBool::new(false));
    let killed_notify = Arc::new(Notify::new());

    let killed_flag_c = Arc::clone(&killed_flag);
    let killed_notify_c = Arc::clone(&killed_notify);
    let kill = options.kill.map(|(match_fn, at)| ProcessKill {
        match_fn,
        at,
        die: Arc::new(move || {
            killed_flag_c.store(true, Ordering::SeqCst);
            killed_notify_c.notify_waiters();
        }),
    });

    let raw = adapters
        .store
        .open(&medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    let kill_armed = kill.is_some();
    let store: Arc<dyn DurableStore> = Arc::new(ProcessStore {
        inner: raw,
        holds: Arc::clone(&holds),
        kill,
        kill_armed: AtomicBool::new(kill_armed),
        dead: AtomicBool::new(false),
    });

    start_epoch_hand(
        store,
        &options.brains,
        &hand_sides,
        Arc::clone(&verifier),
        Arc::clone(&owners),
        Arc::clone(&bodies),
        Arc::clone(&faults),
        &pump,
    )
    .await?;

    Ok(EpochRig {
        adapters,
        medium,
        brains: options.brains,
        verifier,
        hand_sides,
        hosts,
        owners,
        bodies,
        faults,
        holds,
        killed_flag,
        killed_notify,
        pump,
    })
}
