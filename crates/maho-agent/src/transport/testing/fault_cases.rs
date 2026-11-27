//! Port of senpi `packages/agent/src/transport/testing/fault-cases.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};

use super::fault_transport::{
    FAULT_PROFILE_NAMES, FaultProfileName, FaultStats, create_carrier_fault,
};
use super::transport_harness::{
    TransportCaseAdapters, all_delivered, expect_exact_delivery, open_link, range,
};

type FaultCase = CaseImplementation<TransportCaseAdapters>;

const LANES: [&str; 3] = ["a", "b", "c"];
const FRAMES: u64 = 30;
const PACE_MS: i64 = 20;

fn applied(name: FaultProfileName, stats: &FaultStats) -> bool {
    match name {
        FaultProfileName::Slow => stats.refused > 0,
        FaultProfileName::Lossy => stats.lost > 0,
        FaultProfileName::Laggy => stats.reordered > 0,
        FaultProfileName::Flap | FaultProfileName::DropReattach => stats.drops >= 2,
    }
}

async fn run_under_profile(
    name: FaultProfileName,
    adapters: TransportCaseAdapters,
) -> ConformanceResult {
    let stats = Arc::new(Mutex::new(FaultStats::default()));
    let link = open_link(
        &adapters,
        Some(create_carrier_fault(name, Arc::clone(&stats))),
    )?;
    let mut counts = BTreeMap::new();
    for lane in LANES {
        counts.insert(lane.to_owned(), FRAMES);
    }
    let mut sends = Vec::new();
    for n in range(1, FRAMES) {
        for lane in LANES {
            sends.push(link.send(lane, n));
        }
        link.advance(PACE_MS).await?;
    }
    link.run_until(
        || all_delivered(&link, &counts),
        &format!("FAULT-TRANSPORT {}: every frame delivered", name.as_str()),
        600_000,
    )
    .await?;
    link.await_all(sends, &format!("FAULT-TRANSPORT {}", name.as_str()))
        .await?;
    link.advance(2000).await?;
    expect_exact_delivery(
        &link,
        &counts,
        &format!("FAULT-TRANSPORT {}", name.as_str()),
    )?;
    let st = stats.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if !applied(name, &st) {
        return Err(format!(
            "FAULT-TRANSPORT {}: the profile never acted ({st:?})",
            name.as_str()
        ));
    }
    Ok(())
}

pub fn fault_transport_cases() -> BTreeMap<&'static str, FaultCase> {
    let mut map: BTreeMap<&'static str, FaultCase> = BTreeMap::new();
    let ids = [
        "FAULT-TRANSPORT-01",
        "FAULT-TRANSPORT-02",
        "FAULT-TRANSPORT-03",
        "FAULT-TRANSPORT-04",
        "FAULT-TRANSPORT-05",
    ];
    for (id, name) in ids.into_iter().zip(FAULT_PROFILE_NAMES) {
        map.insert(id, Arc::new(move |a| Box::pin(run_under_profile(name, a))));
    }
    map
}

pub const FAULT_TRANSPORT_CASES: fn() -> BTreeMap<&'static str, FaultCase> = fault_transport_cases;
