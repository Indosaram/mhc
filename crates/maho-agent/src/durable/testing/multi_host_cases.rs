//! Port of senpi `packages/agent/src/durable/testing/multi-host-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};

use super::store_harness::expect_equal;
use super::wake_harness::{BootOptions, SessionSlot, WakeCaseAdapters, create_slot};

type MultiHostCase = CaseImplementation<WakeCaseAdapters>;

async fn schedule(slot: &SessionSlot, id: &str, offset: i64) -> Result<(), String> {
    let session = slot
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    let at = slot.base + offset;
    let loop_handle = session.loop_handle;
    session
        .host
        .admit(|| async move { loop_handle.schedule(id, at).await })
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct Observation {
    fires: Vec<usize>,
    ran: Vec<Vec<String>>,
    alarms: Vec<Option<i64>>,
}

async fn observe(slots: &[SessionSlot]) -> Result<Observation, String> {
    let mut fires = Vec::with_capacity(slots.len());
    let mut ran = Vec::with_capacity(slots.len());
    let mut alarms = Vec::with_capacity(slots.len());
    for slot in slots {
        fires.push(slot.fires());
        ran.push(slot.ran().await?);
        alarms.push(slot.alarm_at().await?);
    }
    Ok(Observation { fires, ran, alarms })
}

async fn many_sessions_on_one_host(adapters: WakeCaseAdapters) -> ConformanceResult {
    let first = create_slot(adapters.clone(), 1).await?;
    let second = create_slot(adapters.clone(), 1).await?;
    let third = create_slot(adapters.clone(), 1).await?;
    let slots = vec![first.clone(), second.clone(), third.clone()];
    schedule(&first, "x", 120_000).await?;
    schedule(&first, "y", 300_000).await?;
    schedule(&second, "x", 200_000).await?;
    third
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;

    expect_equal(
        &observe(&slots).await?,
        &Observation {
            fires: vec![0, 0, 0],
            ran: vec![vec![], vec![], vec![]],
            alarms: vec![Some(120_000), Some(200_000), None],
        },
        "armed per session",
    )?;

    second.kill();
    adapters.wake.advance_to(first.base + 120_000).await?;
    expect_equal(
        &observe(&slots).await?,
        &Observation {
            fires: vec![1, 0, 0],
            ran: vec![vec!["x".to_owned()], vec![], vec![]],
            alarms: vec![Some(300_000), Some(200_000), None],
        },
        "a fire resumes only its own session; the same task ID elsewhere is untouched",
    )?;

    third.kill();
    third
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &observe(&slots).await?.alarms,
        &vec![Some(300_000), Some(200_000), None],
        "restarting one session moves no other session's alarm",
    )?;

    adapters.wake.advance_to(first.base + 300_000).await?;
    expect_equal(
        &observe(&slots).await?,
        &Observation {
            fires: vec![2, 1, 0],
            ran: vec![
                vec!["x".to_owned(), "y".to_owned()],
                vec!["x".to_owned()],
                vec![],
            ],
            alarms: vec![None, None, None],
        },
        "an evicted session comes back on its own alarm; records and wakes stay per session",
    )
}

pub fn multi_host_cases() -> BTreeMap<&'static str, MultiHostCase> {
    let mut map: BTreeMap<&'static str, MultiHostCase> = BTreeMap::new();
    map.insert(
        "MULTI-HOST-01",
        Arc::new(|a| Box::pin(many_sessions_on_one_host(a))),
    );
    map
}

pub const MULTI_HOST_CASES: fn() -> BTreeMap<&'static str, MultiHostCase> = multi_host_cases;
