//! Port of senpi `packages/agent/src/execution/testing/fault-hand-cases.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use futures::future::join_all;
use maho_ai::types::{ContentBlock, ToolResultMessage};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::remote_tool_host::HandPlacement;
use crate::transport::testing::fault_transport::{
    FAULT_PROFILE_NAMES, FaultProfileName, FaultStats, create_carrier_fault,
};
use crate::types::ToolExecutionMode;

use super::hand_harness::{HandCaseAdapters, OpenHandRigOptions, call, open_hand_rig};

type HandCase = CaseImplementation<HandCaseAdapters>;

const SCOPES: &[&str] = &["a", "b"];
const CALLS: usize = 12;
const PACE_MS: i64 = 30;

fn ids(scope: &str) -> Vec<String> {
    (1..=CALLS).map(|i| format!("{scope}{i}")).collect()
}

fn texts(messages: &[ToolResultMessage]) -> Vec<String> {
    messages
        .iter()
        .map(|message| {
            message
                .content
                .iter()
                .filter_map(|p| match p {
                    ContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

fn acted(name: FaultProfileName, stats: &FaultStats) -> bool {
    match name {
        FaultProfileName::Slow => stats.refused > 0,
        FaultProfileName::Lossy => stats.lost > 0,
        FaultProfileName::Laggy => stats.reordered > 0,
        FaultProfileName::Flap | FaultProfileName::DropReattach => stats.drops >= 2,
    }
}

async fn under_profile(adapters: HandCaseAdapters, name: FaultProfileName) -> ConformanceResult {
    let stats = Arc::new(Mutex::new(FaultStats::default()));
    let stats_c = Arc::clone(&stats);
    let rig = open_hand_rig(
        adapters,
        OpenHandRigOptions {
            placement: HandPlacement::CrossMachine,
            kill_at: None,
            fault: Some(create_carrier_fault(name, stats_c)),
        },
    )
    .await?;
    let what = format!("FAULT-HAND {}", name.as_str());
    let mut batches = Vec::new();
    for index in 0..CALLS {
        for scope in SCOPES {
            let id = format!("{scope}{}", index + 1);
            batches.push(tokio::spawn(rig.execute(
                scope,
                vec![call(&id)],
                ToolExecutionMode::Sequential,
            )));
        }
        rig.advance(PACE_MS).await;
    }
    let settled = rig
        .settle(join_all(batches), &format!("{what}: every call settled"))
        .await?;
    let mut actual_texts = Vec::new();
    for res in settled {
        let host_res = res.map_err(|e| e.to_string())??;
        actual_texts.extend(texts(&host_res.messages));
    }
    actual_texts.sort();
    let mut expected_texts: Vec<String> = SCOPES
        .iter()
        .flat_map(|s| ids(s).into_iter().map(|id| format!("ran:{id}")))
        .collect();
    expected_texts.sort();
    expect_equal(
        &actual_texts,
        &expected_texts,
        &format!("{what}: results equal the unfaulted run"),
    )?;
    for scope in SCOPES {
        let scope_bodies: Vec<String> = rig
            .bodies()
            .into_iter()
            .filter(|b| b.starts_with(&format!("{scope}:")))
            .collect();
        let expected_bodies: Vec<String> = ids(scope)
            .into_iter()
            .map(|id| format!("{scope}:{id}"))
            .collect();
        expect_equal(
            &scope_bodies,
            &expected_bodies,
            &format!("{what}: scope {scope} ran each call once, in lane order"),
        )?;
    }
    expect_equal(
        &rig.faults(),
        &Vec::<String>::new(),
        &format!("{what}: the hand reported no fault"),
    )?;
    let final_stats = stats.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if !acted(name, &final_stats) {
        return Err(format!(
            "{what}: the profile never acted ({})",
            serde_json::to_string(&final_stats).unwrap_or_default()
        ));
    }
    Ok(())
}

async fn unreachable_then_back(adapters: HandCaseAdapters) -> ConformanceResult {
    let rig = open_hand_rig(
        adapters,
        OpenHandRigOptions {
            placement: HandPlacement::CrossMachine,
            kill_at: None,
            fault: None,
        },
    )
    .await?;
    rig.gate_bodies();
    let settled_scopes = Arc::new(Mutex::new(BTreeSet::new()));
    let mut batches = Vec::new();
    for scope in SCOPES {
        let fut = rig.execute(
            scope,
            vec![call(&format!("{scope}1"))],
            ToolExecutionMode::Parallel,
        );
        let scopes_c = Arc::clone(&settled_scopes);
        let s_name = (*scope).to_string();
        batches.push(tokio::spawn(async move {
            let res = fut.await;
            scopes_c
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(s_name);
            res
        }));
    }
    rig.run_until(
        || rig.bodies().len() == SCOPES.len(),
        "FAULT-HAND-MULTI-01: every scope's call is running",
    )
    .await?;
    rig.disconnect();
    for scope in SCOPES {
        rig.release(&format!("{scope}1"));
    }
    rig.advance(60_000).await;
    let settled_now: Vec<String> = settled_scopes
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .cloned()
        .collect();
    expect_equal(
        &settled_now,
        &Vec::<String>::new(),
        "FAULT-HAND-MULTI-01: dependent calls pause while the hand is unreachable",
    )?;
    rig.reconnect();
    let settled = rig
        .settle(
            join_all(batches),
            "FAULT-HAND-MULTI-01: every scope resumed after reconnect",
        )
        .await?;
    let mut actual_texts = Vec::new();
    for res in settled {
        let host_res = res.map_err(|e| e.to_string())??;
        actual_texts.push(texts(&host_res.messages));
    }
    let expected_texts: Vec<Vec<String>> =
        SCOPES.iter().map(|s| vec![format!("ran:{s}1")]).collect();
    expect_equal(
        &actual_texts,
        &expected_texts,
        "FAULT-HAND-MULTI-01: each scope settled with its own result",
    )?;
    rig.advance(5_000).await;
    let expected_bodies: Vec<String> = SCOPES.iter().map(|s| format!("{s}:{s}1")).collect();
    expect_equal(
        &rig.bodies(),
        &expected_bodies,
        "FAULT-HAND-MULTI-01: resumed once: no body ran twice",
    )?;
    expect_equal(
        &rig.faults(),
        &Vec::<String>::new(),
        "FAULT-HAND-MULTI-01: the hand reported no fault",
    )?;
    Ok(())
}

pub fn fault_hand_cases() -> BTreeMap<&'static str, HandCase> {
    let mut map: BTreeMap<&'static str, HandCase> = BTreeMap::new();
    for (index, &name) in FAULT_PROFILE_NAMES.iter().enumerate() {
        let id: &'static str = Box::leak(format!("FAULT-HAND-0{}", index + 1).into_boxed_str());
        map.insert(id, Arc::new(move |a| Box::pin(under_profile(a, name))));
    }
    map.insert(
        "FAULT-HAND-MULTI-01",
        Arc::new(|a| Box::pin(unreachable_then_back(a))),
    );
    map
}

pub const FAULT_HAND_CASES: fn() -> BTreeMap<&'static str, HandCase> = fault_hand_cases;
