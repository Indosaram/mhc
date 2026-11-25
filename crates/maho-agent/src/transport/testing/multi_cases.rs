//! Port of senpi `packages/agent/src/transport/testing/multi-cases.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::transport::in_process_carrier::CarrierLink;
use crate::transport::transport::{CarrierConnection, CarrierMessage, CarrierSide};

use super::fault_transport::{FaultProfileName, FaultStats, create_carrier_fault};
use super::transport_harness::{
    CarrierFault, TransportCaseAdapters, TransportConformanceAdapter, WireView, all_delivered,
    expect_exact_delivery, open_link, range,
};

type MultiCase = CaseImplementation<TransportCaseAdapters>;

async fn many_lanes(adapters: TransportCaseAdapters) -> ConformanceResult {
    let link = open_link(&adapters, None)?;
    let lanes: Vec<String> = range(1, 8)
        .into_iter()
        .map(|n| format!("scope-{n}"))
        .collect();
    let mut counts = BTreeMap::new();
    for lane in &lanes {
        counts.insert(lane.clone(), 25u64);
    }
    let mut sends = Vec::new();
    for n in range(1, 25) {
        for lane in &lanes {
            sends.push(link.send(lane, n));
        }
    }
    link.run_until(
        || all_delivered(&link, &counts),
        "MULTI-LANE-01 every scope delivered",
        120_000,
    )
    .await?;
    link.await_all(sends, "MULTI-LANE-01").await?;
    link.advance(1000).await?;
    expect_exact_delivery(&link, &counts, "MULTI-LANE-01")?;

    let lanes_in_log: Vec<String> = link
        .log()
        .into_iter()
        .filter_map(|entry| entry.split(':').next().map(str::to_owned))
        .collect();
    for lane in &lanes {
        let first = lanes_in_log.iter().position(|l| l == lane);
        let last_of_others = lanes
            .iter()
            .filter(|other| *other != lane)
            .filter_map(|other| lanes_in_log.iter().rposition(|l| l == other))
            .min();
        if let (Some(f), Some(last)) = (first, last_of_others)
            && f > last
        {
            return Err(format!(
                "MULTI-LANE-01: {lane} started only after another scope had finished"
            ));
        }
    }
    Ok(())
}

async fn stalled_lane_does_not_hold_others(adapters: &TransportCaseAdapters) -> ConformanceResult {
    let link = open_link(adapters, None)?;
    let release = link.gate("A");
    let a_sends: Vec<_> = range(1, 100)
        .into_iter()
        .map(|n| link.send("A", n))
        .collect();
    let b_sends: Vec<_> = range(1, 10)
        .into_iter()
        .map(|n| link.send("B", n))
        .collect();
    let mut b_counts = BTreeMap::new();
    b_counts.insert("B".to_owned(), 10u64);
    link.run_until(
        || all_delivered(&link, &b_counts),
        "MULTI-FAIR-01 B delivered while A's receiver is stalled",
        20_000,
    )
    .await?;
    link.await_all(b_sends, "MULTI-FAIR-01 B").await?;
    if !link.delivered("A").is_empty() {
        return Err("MULTI-FAIR-01: A delivered through a stalled receiver".to_owned());
    }
    release();
    let mut a_counts = BTreeMap::new();
    a_counts.insert("A".to_owned(), 100u64);
    link.run_until(
        || all_delivered(&link, &a_counts),
        "MULTI-FAIR-01 A delivered after release",
        120_000,
    )
    .await?;
    link.await_all(a_sends, "MULTI-FAIR-01 A").await?;
    link.advance(1000).await?;
    let mut both = BTreeMap::new();
    both.insert("A".to_owned(), 100u64);
    both.insert("B".to_owned(), 10u64);
    expect_exact_delivery(&link, &both, "MULTI-FAIR-01 stalled lane")
}

const MAX_A_FRAMES_BEFORE_B: usize = 4;

async fn long_lane_does_not_starve_another(adapters: &TransportCaseAdapters) -> ConformanceResult {
    let stats = Arc::new(Mutex::new(FaultStats::default()));
    let link = open_link(
        adapters,
        Some(create_carrier_fault(
            FaultProfileName::Slow,
            Arc::clone(&stats),
        )),
    )?;
    let mut sends: Vec<_> = range(1, 300)
        .into_iter()
        .map(|n| link.send("A", n))
        .collect();
    link.run_until(
        || link.delivered("A").len() >= 20,
        "MULTI-FAIR-01 A's long lane under way",
        120_000,
    )
    .await?;
    let before = link.delivered("A").len();
    let b_send = link.send("B", 1);
    sends.push(b_send);
    let mut b_counts = BTreeMap::new();
    b_counts.insert("B".to_owned(), 1u64);
    link.run_until(
        || all_delivered(&link, &b_counts),
        "MULTI-FAIR-01 B delivered behind A's long lane",
        120_000,
    )
    .await?;
    let ahead_of_b = link.delivered("A").len() - before;
    if ahead_of_b > MAX_A_FRAMES_BEFORE_B {
        return Err(format!(
            "MULTI-FAIR-01: {ahead_of_b} of A's frames went ahead of B's one frame (at most {MAX_A_FRAMES_BEFORE_B})"
        ));
    }
    let mut a_counts = BTreeMap::new();
    a_counts.insert("A".to_owned(), 300u64);
    link.run_until(
        || all_delivered(&link, &a_counts),
        "MULTI-FAIR-01 A's long lane delivered",
        600_000,
    )
    .await?;
    link.await_all(sends, "MULTI-FAIR-01 long lane").await?;
    link.advance(1000).await?;
    let mut both = BTreeMap::new();
    both.insert("A".to_owned(), 300u64);
    both.insert("B".to_owned(), 1u64);
    expect_exact_delivery(&link, &both, "MULTI-FAIR-01 long lane")?;
    if stats.lock().unwrap_or_else(|p| p.into_inner()).refused == 0 {
        return Err("MULTI-FAIR-01: the slow carrier never pushed back".to_owned());
    }
    Ok(())
}

async fn fairness(adapters: TransportCaseAdapters) -> ConformanceResult {
    stalled_lane_does_not_hold_others(&adapters).await?;
    long_lane_does_not_starve_another(&adapters).await
}

struct LoseOnceFault {
    adapter: Arc<dyn TransportConformanceAdapter>,
    lane: String,
    seq: u64,
    stats: Arc<Mutex<FaultStats>>,
}

struct LoseOnceConnection {
    inner: Arc<dyn CarrierConnection>,
    adapter: Arc<dyn TransportConformanceAdapter>,
    lane: String,
    seq: u64,
    stats: Arc<Mutex<FaultStats>>,
}

impl CarrierConnection for LoseOnceConnection {
    fn send(&self, message: CarrierMessage) -> bool {
        if let WireView::Data { lane, seq } = self.adapter.decode(&message)
            && lane == self.lane
            && seq == self.seq
        {
            let mut st = self.stats.lock().unwrap_or_else(|p| p.into_inner());
            if st.lost == 0 {
                st.lost += 1;
                return true;
            }
        }
        self.inner.send(message)
    }

    fn close(&self) {
        self.inner.close();
    }
}

struct LoseOnceSide {
    inner: Arc<dyn CarrierSide>,
    adapter: Arc<dyn TransportConformanceAdapter>,
    lane: String,
    seq: u64,
    stats: Arc<Mutex<FaultStats>>,
}

impl CarrierSide for LoseOnceSide {
    fn attached(&self, connection: Arc<dyn CarrierConnection>) {
        self.inner.attached(Arc::new(LoseOnceConnection {
            inner: connection,
            adapter: Arc::clone(&self.adapter),
            lane: self.lane.clone(),
            seq: self.seq,
            stats: Arc::clone(&self.stats),
        }));
    }

    fn message(&self, message: &CarrierMessage) {
        self.inner.message(message);
    }

    fn writable(&self) {
        self.inner.writable();
    }

    fn detached(&self) {
        self.inner.detached();
    }
}

impl CarrierFault for LoseOnceFault {
    fn wrap(&self, side: Arc<dyn CarrierSide>) -> Arc<dyn CarrierSide> {
        Arc::new(LoseOnceSide {
            inner: side,
            adapter: Arc::clone(&self.adapter),
            lane: self.lane.clone(),
            seq: self.seq,
            stats: Arc::clone(&self.stats),
        })
    }

    fn start(&self, _link: &CarrierLink) {}
}

const LONG_LANE_FRAMES: u64 = 400;
const LONG_LANE_PACE_MS: i64 = 10;

async fn loss_on_one_lane_is_not_held_by_another(
    adapters: TransportCaseAdapters,
) -> ConformanceResult {
    let stats = Arc::new(Mutex::new(FaultStats::default()));
    let decode_adapter = Arc::clone(&adapters.transport);
    let stats_fault = Arc::clone(&stats);
    let link = open_link(
        &adapters,
        Some(Box::new(move |_| {
            Arc::new(LoseOnceFault {
                adapter: decode_adapter,
                lane: "B".to_owned(),
                seq: 1,
                stats: stats_fault,
            })
        })),
    )?;
    link.pace("A", LONG_LANE_PACE_MS);
    let mut sends: Vec<_> = range(1, LONG_LANE_FRAMES)
        .into_iter()
        .map(|n| link.send("A", n))
        .collect();
    link.run_until(
        || link.delivered("A").len() >= 10,
        "MULTI-FAIR-03 A's long lane under way",
        120_000,
    )
    .await?;
    let before = link.delivered("A").len();
    let b_send = link.send("B", 1);
    sends.push(b_send);
    let mut b_counts = BTreeMap::new();
    b_counts.insert("B".to_owned(), 1u64);
    link.run_until(
        || all_delivered(&link, &b_counts),
        "MULTI-FAIR-03 B delivered after its loss",
        600_000,
    )
    .await?;
    let lost_count = stats.lock().unwrap_or_else(|p| p.into_inner()).lost;
    if lost_count != 1 {
        let st = stats.lock().unwrap_or_else(|p| p.into_inner()).clone();
        return Err(format!(
            "MULTI-FAIR-03: B#1 was not lost on the carrier ({st:?})"
        ));
    }
    let ahead_of_b = link.delivered("A").len() - before;
    if ahead_of_b >= (LONG_LANE_FRAMES as usize) / 2 {
        return Err(format!(
            "MULTI-FAIR-03: B's lost frame was resent only after {ahead_of_b} of A's frames (fewer than {})",
            LONG_LANE_FRAMES / 2
        ));
    }
    let mut a_counts = BTreeMap::new();
    a_counts.insert("A".to_owned(), LONG_LANE_FRAMES);
    link.run_until(
        || all_delivered(&link, &a_counts),
        "MULTI-FAIR-03 A delivered",
        600_000,
    )
    .await?;
    link.await_all(sends, "MULTI-FAIR-03").await?;
    link.advance(2000).await?;
    let mut both = BTreeMap::new();
    both.insert("A".to_owned(), LONG_LANE_FRAMES);
    both.insert("B".to_owned(), 1u64);
    expect_exact_delivery(&link, &both, "MULTI-FAIR-03")
}

pub fn multi_cases() -> BTreeMap<&'static str, MultiCase> {
    let mut map: BTreeMap<&'static str, MultiCase> = BTreeMap::new();
    map.insert("MULTI-LANE-01", Arc::new(|a| Box::pin(many_lanes(a))));
    map.insert("MULTI-FAIR-01", Arc::new(|a| Box::pin(fairness(a))));
    map.insert(
        "MULTI-FAIR-03",
        Arc::new(|a| Box::pin(loss_on_one_lane_is_not_held_by_another(a))),
    );
    map
}

pub const MULTI_CASES: fn() -> BTreeMap<&'static str, MultiCase> = multi_cases;
