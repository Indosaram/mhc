//! Port of senpi `packages/agent/src/transport/testing/transport-harness.ts`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

use crate::durable::testing::manual_timers::{ManualTimers, create_manual_timers};
use crate::durable::testing::store_harness::expect_equal;
use crate::transport::in_process_carrier::{
    CarrierLink, InProcessNetworkOptions, create_in_process_network,
};
use crate::transport::local_endpoint::local_endpoint;
use crate::transport::transport::{
    CarrierConnection, CarrierMessage, CarrierSide, TransportClock, TransportClosedError,
    TransportFrame, TransportSide,
};

/// What a conformance host's decoder reports about one carrier message; data and ACK sequence numbers start at 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireView {
    Data { lane: String, seq: u64 },
    Ack { lane: String, seq: u64 },
    Other,
}

/// What a host provides to the TRANSPORT, MULTI-LANE/FAIR and FAULT-TRANSPORT cases.
pub trait TransportConformanceAdapter: Send + Sync {
    /// A new transport endpoint scheduling on `clock`; the cases attach two of them through an in-process carrier.
    fn create_side(&self, clock: Arc<dyn TransportClock>) -> Arc<dyn TransportSide>;
    /// Reads the lane and sequence number of a carrier message, so the cases can check resume from the last ACK.
    fn decode(&self, message: &CarrierMessage) -> WireView;
}

#[derive(Clone)]
pub struct TransportCaseAdapters {
    pub transport: Arc<dyn TransportConformanceAdapter>,
    pub platform: String,
}

/// A fault on the carrier path: it wraps each side's view of the carrier and may drive the link.
pub trait CarrierFault: Send + Sync {
    fn wrap(&self, side: Arc<dyn CarrierSide>) -> Arc<dyn CarrierSide>;
    fn start(&self, link: &CarrierLink);
}

const TICK_MS: i64 = 5;
const DRAIN_TURNS: usize = 32;

async fn drain_microtasks() {
    for _ in 0..DRAIN_TURNS {
        tokio::task::yield_now().await;
    }
}

struct TapConnection {
    inner: Arc<dyn CarrierConnection>,
    adapter: Arc<dyn TransportConformanceAdapter>,
    acked: Arc<Mutex<HashMap<String, u64>>>,
    violations: Arc<Mutex<Vec<String>>>,
}

impl CarrierConnection for TapConnection {
    fn send(&self, message: CarrierMessage) -> bool {
        if let WireView::Data { lane, seq } = self.adapter.decode(&message) {
            let last_acked = self
                .acked
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&lane)
                .copied()
                .unwrap_or(0);
            if seq <= last_acked {
                self.violations
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(format!("{lane}#{seq} sent again after its ACK"));
            }
        }
        self.inner.send(message)
    }

    fn close(&self) {
        self.inner.close();
    }
}

struct TapSide {
    side: Arc<dyn TransportSide>,
    adapter: Arc<dyn TransportConformanceAdapter>,
    acked: Arc<Mutex<HashMap<String, u64>>>,
    violations: Arc<Mutex<Vec<String>>>,
}

impl CarrierSide for TapSide {
    fn attached(&self, connection: Arc<dyn CarrierConnection>) {
        self.side.attached(Arc::new(TapConnection {
            inner: connection,
            adapter: Arc::clone(&self.adapter),
            acked: Arc::clone(&self.acked),
            violations: Arc::clone(&self.violations),
        }));
    }

    fn message(&self, message: &CarrierMessage) {
        if let WireView::Ack { lane, seq } = self.adapter.decode(message) {
            let mut guard = self.acked.lock().unwrap_or_else(|p| p.into_inner());
            let prev = guard.get(&lane).copied().unwrap_or(0);
            if seq > prev {
                guard.insert(lane, seq);
            }
        }
        self.side.message(message);
    }

    fn writable(&self) {
        self.side.writable();
    }

    fn detached(&self) {
        self.side.detached();
    }
}

struct GateState {
    open: AtomicBool,
    notify: Notify,
}

#[derive(Clone)]
pub struct TestLink {
    pub sender: Arc<dyn TransportSide>,
    pub link: CarrierLink,
    pub timers: ManualTimers,
    log: Arc<Mutex<Vec<String>>>,
    violations: Arc<Mutex<Vec<String>>>,
    received: Arc<Mutex<HashMap<String, Vec<u64>>>>,
    gates: Arc<Mutex<HashMap<String, Arc<GateState>>>>,
    paces: Arc<Mutex<HashMap<String, i64>>>,
}

impl TestLink {
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn violations(&self) -> Vec<String> {
        self.violations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn send(&self, lane: &str, n: u64) -> JoinHandle<Result<(), TransportClosedError>> {
        let sender = Arc::clone(&self.sender);
        let lane_owned = lane.to_owned();
        let payload = n.to_string().into_bytes();
        tokio::spawn(async move { sender.send(&lane_owned, payload).await })
    }

    pub fn send_then_push(
        &self,
        lane: &str,
        n: u64,
        resolved: Arc<Mutex<Vec<u64>>>,
    ) -> JoinHandle<Result<(), TransportClosedError>> {
        let sender = Arc::clone(&self.sender);
        let lane_owned = lane.to_owned();
        let payload = n.to_string().into_bytes();
        tokio::spawn(async move {
            let res = sender.send(&lane_owned, payload).await;
            if res.is_ok() {
                resolved.lock().unwrap_or_else(|p| p.into_inner()).push(n);
            }
            res
        })
    }

    pub fn delivered(&self, lane: &str) -> Vec<u64> {
        self.received
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(lane)
            .cloned()
            .unwrap_or_default()
    }

    pub fn gate(&self, lane: &str) -> Box<dyn FnOnce() + Send> {
        let state = Arc::new(GateState {
            open: AtomicBool::new(false),
            notify: Notify::new(),
        });
        self.gates
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(lane.to_owned(), Arc::clone(&state));
        let gates = Arc::clone(&self.gates);
        let lane_owned = lane.to_owned();
        Box::new(move || {
            gates
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&lane_owned);
            state.open.store(true, Ordering::SeqCst);
            state.notify.notify_waiters();
        })
    }

    pub fn pace(&self, lane: &str, ms: i64) {
        self.paces
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(lane.to_owned(), ms);
    }

    pub async fn advance(&self, ms: i64) -> Result<(), String> {
        drain_microtasks().await;
        let end = self.timers.now() + ms;
        while self.timers.now() < end {
            let next = end.min(self.timers.now() + TICK_MS);
            self.timers.advance_to(next).await?;
            drain_microtasks().await;
        }
        Ok(())
    }

    pub async fn run_until<F>(&self, done: F, what: &str, budget_ms: i64) -> Result<(), String>
    where
        F: Fn() -> bool,
    {
        drain_microtasks().await;
        let end = self.timers.now() + budget_ms;
        while !done() {
            if self.timers.now() >= end {
                let counts: Vec<String> = self
                    .received
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .iter()
                    .map(|(lane, ns)| format!("{lane}={}", ns.len()))
                    .collect();
                return Err(format!(
                    "{what}: not reached within {budget_ms} ms of transport time (delivered {})",
                    counts.join(" ")
                ));
            }
            self.advance(TICK_MS).await?;
        }
        Ok(())
    }

    pub async fn await_all(
        &self,
        handles: Vec<JoinHandle<Result<(), TransportClosedError>>>,
        what: &str,
    ) -> Result<(), String> {
        let total = handles.len();
        let settled = Arc::new(AtomicU64::new(0));
        let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        for handle in handles {
            let settled_clone = Arc::clone(&settled);
            let failure_clone = Arc::clone(&failure);
            tokio::spawn(async move {
                match handle.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        let mut guard = failure_clone.lock().unwrap_or_else(|p| p.into_inner());
                        if guard.is_none() {
                            *guard = Some(e.to_string());
                        }
                    }
                    Err(e) => {
                        let mut guard = failure_clone.lock().unwrap_or_else(|p| p.into_inner());
                        if guard.is_none() {
                            *guard = Some(format!("task join error: {e}"));
                        }
                    }
                }
                settled_clone.fetch_add(1, Ordering::SeqCst);
            });
        }
        self.run_until(
            || settled.load(Ordering::SeqCst) as usize == total,
            &format!("{what}: every send settled"),
            120_000,
        )
        .await?;
        if let Some(err) = failure.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Err(err);
        }
        Ok(())
    }
}

pub fn open_link(
    adapters: &TransportCaseAdapters,
    fault: Option<super::fault_transport::FaultBuilder>,
) -> Result<TestLink, String> {
    let timers = create_manual_timers(0);
    let timers_for_net = timers.clone();
    let network = create_in_process_network(InProcessNetworkOptions {
        schedule: Some(Arc::new(move |deliver| {
            let _ = timers_for_net.set_timer_sync(deliver, 0);
        })),
    });
    let clock: Arc<dyn TransportClock> = Arc::new(timers.clone());
    let sender = adapters.transport.create_side(Arc::clone(&clock));
    let receiver = adapters.transport.create_side(clock);
    let violations = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(Mutex::new(Vec::new()));
    let received: Arc<Mutex<HashMap<String, Vec<u64>>>> = Arc::new(Mutex::new(HashMap::new()));
    let gates: Arc<Mutex<HashMap<String, Arc<GateState>>>> = Arc::new(Mutex::new(HashMap::new()));
    let paces: Arc<Mutex<HashMap<String, i64>>> = Arc::new(Mutex::new(HashMap::new()));
    let installed = fault.map(|build| build(&timers));

    let outer = |side: Arc<dyn TransportSide>| -> Arc<dyn CarrierSide> {
        let tapped: Arc<dyn CarrierSide> = Arc::new(TapSide {
            side,
            adapter: Arc::clone(&adapters.transport),
            acked: Arc::new(Mutex::new(HashMap::new())),
            violations: Arc::clone(&violations),
        });
        match &installed {
            Some(f) => f.wrap(tapped),
            None => tapped,
        }
    };

    let endpoint = local_endpoint(
        &adapters.platform,
        "transport-conformance",
        "/run/senpi-conformance",
    )?;
    let _ = network.listen(&endpoint, outer(Arc::clone(&receiver)));
    let link = network.dial(&endpoint, outer(Arc::clone(&sender)))?;

    let gates_recv = Arc::clone(&gates);
    let paces_recv = Arc::clone(&paces);
    let timers_recv = timers.clone();
    let received_recv = Arc::clone(&received);
    let log_recv = Arc::clone(&log);

    receiver.on_frame(Arc::new(move |frame: TransportFrame| {
        let gates = Arc::clone(&gates_recv);
        let paces = Arc::clone(&paces_recv);
        let timers = timers_recv.clone();
        let received = Arc::clone(&received_recv);
        let log = Arc::clone(&log_recv);
        Box::pin(async move {
            let gate = gates
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&frame.lane)
                .cloned();
            if let Some(gate) = gate {
                while !gate.open.load(Ordering::SeqCst) {
                    gate.notify.notified().await;
                }
            }
            let pace_ms = paces
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&frame.lane)
                .copied();
            if let Some(ms) = pace_ms {
                let (tx, rx) = oneshot::channel();
                let _ = timers.set_timer_sync(
                    Box::new(move || {
                        let _ = tx.send(());
                    }),
                    ms,
                );
                let _ = rx.await;
            }
            let n: u64 = String::from_utf8_lossy(&frame.payload)
                .trim()
                .parse()
                .unwrap_or(0);
            received
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .entry(frame.lane.clone())
                .or_default()
                .push(n);
            log.lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(format!("{}:{n}", frame.lane));
        })
    }));

    link.connect();
    if let Some(f) = &installed {
        f.start(&link);
    }

    Ok(TestLink {
        sender,
        link,
        timers,
        log,
        violations,
        received,
        gates,
        paces,
    })
}

pub fn range(from: u64, to: u64) -> Vec<u64> {
    (from..=to).collect()
}

pub fn expect_exact_delivery(
    link: &TestLink,
    counts: &BTreeMap<String, u64>,
    what: &str,
) -> Result<(), String> {
    let actual_delivered: Vec<Vec<u64>> = counts.keys().map(|l| link.delivered(l)).collect();
    let expected_delivered: Vec<Vec<u64>> = counts.values().map(|&c| range(1, c)).collect();
    expect_equal(
        &actual_delivered,
        &expected_delivered,
        &format!("{what}: each lane delivered in order, once"),
    )?;
    let actual_in_flight: Vec<usize> = counts.keys().map(|l| link.sender.in_flight(l)).collect();
    let expected_in_flight: Vec<usize> = vec![0; counts.len()];
    expect_equal(
        &actual_in_flight,
        &expected_in_flight,
        &format!("{what}: nothing in flight"),
    )?;
    expect_equal(
        &link.violations(),
        &Vec::<String>::new(),
        &format!("{what}: resume starts after each lane's last ACK"),
    )
}

#[allow(dead_code)]
#[derive(Serialize)]
struct StatsSnapshot {
    delivered: BTreeMap<String, usize>,
}

pub fn all_delivered(link: &TestLink, counts: &BTreeMap<String, u64>) -> bool {
    counts
        .iter()
        .all(|(lane, &count)| link.delivered(lane).len() as u64 >= count)
}
