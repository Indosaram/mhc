//! Port of senpi `packages/agent/src/transport/testing/transport-cases.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;

use super::transport_harness::{
    TransportCaseAdapters, all_delivered, expect_exact_delivery, open_link, range,
};

type TransportCase = CaseImplementation<TransportCaseAdapters>;

async fn order(adapters: TransportCaseAdapters) -> ConformanceResult {
    let link = open_link(&adapters, None)?;
    let mut counts = BTreeMap::new();
    for lane in ["a", "b", "c"] {
        counts.insert(lane.to_owned(), 40);
    }
    let mut sends = Vec::new();
    for n in range(1, 40) {
        for lane in ["a", "b", "c"] {
            sends.push(link.send(lane, n));
        }
    }
    link.run_until(
        || all_delivered(&link, &counts),
        "TRANSPORT-01 every frame delivered",
        120_000,
    )
    .await?;
    link.await_all(sends, "TRANSPORT-01").await?;
    link.advance(1000).await?;
    expect_exact_delivery(&link, &counts, "TRANSPORT-01")
}

async fn resume(adapters: TransportCaseAdapters) -> ConformanceResult {
    let link = open_link(&adapters, None)?;
    let mut counts = BTreeMap::new();
    counts.insert("a".to_owned(), 30);
    counts.insert("b".to_owned(), 30);
    let mut sends = Vec::new();
    for n in range(1, 10) {
        sends.push(link.send("a", n));
        sends.push(link.send("b", n));
    }
    link.run_until(
        || link.delivered("a").len() >= 4,
        "TRANSPORT-02 first frames delivered",
        120_000,
    )
    .await?;
    link.link.drop_link();
    for n in range(11, 20) {
        sends.push(link.send("a", n));
        sends.push(link.send("b", n));
    }
    link.advance(500).await?;
    if link.sender.in_flight("a") + link.sender.in_flight("b") == 0 {
        return Err(
            "TRANSPORT-02: expected unacknowledged frames while the carrier is down".to_owned(),
        );
    }
    link.link.connect();
    link.run_until(
        || link.delivered("a").len() >= 15,
        "TRANSPORT-02 resumed after the first re-attach",
        120_000,
    )
    .await?;
    link.link.drop_link();
    for n in range(21, 30) {
        sends.push(link.send("a", n));
        sends.push(link.send("b", n));
    }
    link.link.connect();
    link.run_until(
        || all_delivered(&link, &counts),
        "TRANSPORT-02 every frame delivered after two re-attaches",
        120_000,
    )
    .await?;
    link.await_all(sends, "TRANSPORT-02").await?;
    link.advance(1000).await?;
    expect_exact_delivery(&link, &counts, "TRANSPORT-02")
}

async fn backpressure(adapters: TransportCaseAdapters) -> ConformanceResult {
    let link = open_link(&adapters, None)?;
    let release = link.gate("slow");
    let resolved = Arc::new(Mutex::new(Vec::<u64>::new()));
    let sends: Vec<_> = range(1, 200)
        .into_iter()
        .map(|n| link.send_then_push("slow", n, Arc::clone(&resolved)))
        .collect();
    link.advance(2000).await?;
    let admitted = resolved.lock().unwrap_or_else(|p| p.into_inner()).len();
    if admitted == 0 || admitted >= 200 {
        return Err(format!(
            "TRANSPORT-03: a stalled receiver must bound the senders, but {admitted} of 200 sends resolved"
        ));
    }
    link.advance(2000).await?;
    let current_len = resolved.lock().unwrap_or_else(|p| p.into_inner()).len();
    expect_equal(
        &current_len,
        &admitted,
        "TRANSPORT-03 the bound holds while the receiver stays stalled",
    )?;
    expect_equal(
        &link.delivered("slow"),
        &Vec::<u64>::new(),
        "TRANSPORT-03 nothing delivered while the receiver is stalled",
    )?;
    if link.sender.in_flight("slow") > admitted {
        return Err("TRANSPORT-03: more in flight than admitted".to_owned());
    }
    release();
    let mut expected = BTreeMap::new();
    expected.insert("slow".to_owned(), 200);
    link.run_until(
        || all_delivered(&link, &expected),
        "TRANSPORT-03 every frame delivered after release",
        120_000,
    )
    .await?;
    link.await_all(sends, "TRANSPORT-03").await?;
    let resolved_vec = resolved.lock().unwrap_or_else(|p| p.into_inner()).clone();
    expect_equal(
        &resolved_vec,
        &range(1, 200),
        "TRANSPORT-03 sends resolve in send order",
    )?;
    link.advance(1000).await?;
    expect_exact_delivery(&link, &expected, "TRANSPORT-03")
}

pub fn transport_cases() -> BTreeMap<&'static str, TransportCase> {
    let mut map: BTreeMap<&'static str, TransportCase> = BTreeMap::new();
    map.insert("TRANSPORT-01", Arc::new(|a| Box::pin(order(a))));
    map.insert("TRANSPORT-02", Arc::new(|a| Box::pin(resume(a))));
    map.insert("TRANSPORT-03", Arc::new(|a| Box::pin(backpressure(a))));
    map
}

pub const TRANSPORT_CASES: fn() -> BTreeMap<&'static str, TransportCase> = transport_cases;
