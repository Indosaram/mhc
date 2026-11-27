//! Port of senpi `packages/agent/src/execution/testing/kill-epoch.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;

use super::epoch_cases::texts;
use super::epoch_harness::{
    CommitMatch, EpochBrainSpec, OpenEpochRigOptions, floor_commit, open_epoch_rig, step_commit,
};
use super::hand_harness::{HandCaseAdapters, KillAt, call};

#[derive(Debug, Clone)]
pub struct KillEpochDeclaration {
    pub id: &'static str,
    pub expected: &'static str,
}

#[derive(Clone)]
struct KillPoint {
    id: &'static str,
    match_fn: CommitMatch,
    at: KillAt,
    floor_after_kill: u64,
    new_call_runs: bool,
    expected: &'static str,
}

fn old_brain() -> EpochBrainSpec {
    EpochBrainSpec {
        name: "brain-old".to_string(),
        epoch: 1,
    }
}

fn new_brain() -> EpochBrainSpec {
    EpochBrainSpec {
        name: "brain-new".to_string(),
        epoch: 2,
    }
}

fn points() -> Vec<KillPoint> {
    vec![
        KillPoint {
            id: "KILL-EPOCH-01-B",
            match_fn: floor_commit("s", 2),
            at: KillAt::Before,
            floor_after_kill: 1,
            new_call_runs: true,
            expected: "kill before the floor raise is durable: no ACK; the re-sent floor is acked after restart, the new call runs once, the old owner stays fenced",
        },
        KillPoint {
            id: "KILL-EPOCH-01-D",
            match_fn: floor_commit("s", 2),
            at: KillAt::Durable,
            floor_after_kill: 2,
            new_call_runs: true,
            expected: "kill once the floor is durable, before its ACK: the restarted hand holds the floor, the new call runs once, the old owner stays fenced",
        },
        KillPoint {
            id: "KILL-EPOCH-02-B",
            match_fn: step_commit("brain-new", "admit"),
            at: KillAt::Before,
            floor_after_kill: 2,
            new_call_runs: true,
            expected: "kill after the ACK, before the new owner's first call is recorded: the floor survives, the call runs once, the old owner stays fenced",
        },
        KillPoint {
            id: "KILL-EPOCH-02-D",
            match_fn: step_commit("brain-new", "admit"),
            at: KillAt::Durable,
            floor_after_kill: 2,
            new_call_runs: false,
            expected: "kill once the new owner's first call is recorded: the floor survives, the call ends hand_restarted and never runs, the old owner stays fenced",
        },
    ]
}

async fn run_kill_epoch_case(adapters: HandCaseAdapters, point: KillPoint) -> ConformanceResult {
    let what = point.id;
    let old = old_brain();
    let new = new_brain();
    let rig = open_epoch_rig(
        adapters,
        OpenEpochRigOptions {
            brains: vec![old.clone(), new.clone()],
            placement: None,
            kill: Some((point.match_fn.clone(), point.at)),
        },
    )
    .await?;
    rig.set_owner("s", &old.name);
    rig.settle(
        rig.execute(&old.name, "s", vec![call("c0")]),
        &format!("{what}: the old owner's first call"),
    )
    .await??;
    rig.set_owner("s", &new.name);
    let moved_handle = tokio::spawn(rig.execute(&new.name, "s", vec![call("c1")]));
    rig.wait_killed().await?;
    expect_equal(
        &rig.floor("s").await?,
        &Some(json!({
            "epoch": point.floor_after_kill,
            "owner": if point.floor_after_kill == new.epoch { new.name.clone() } else { old.name.clone() },
        })),
        &format!("{what}: the floor durable at the kill"),
    )?;
    rig.restart().await?;
    let result = rig
        .settle(
            async move { moved_handle.await.map_err(|e| e.to_string())? },
            &format!("{what}: the new owner's call"),
        )
        .await??;
    if point.new_call_runs {
        expect_equal(
            &texts(&result),
            &vec!["ran:c1".to_string()],
            &format!("{what}: the new owner's call ran"),
        )?;
    } else {
        expect_equal(
            &result.messages.first().and_then(|m| m.details.clone()),
            &Some(json!({
                "outcome": "hand_restarted",
                "uncertain": false,
                "hooksRan": false,
            })),
            &format!("{what}: the recorded call ended hand_restarted, not uncertain"),
        )?;
    }
    let delayed = rig
        .settle(
            rig.execute(&old.name, "s", vec![call("c2")]),
            &format!("{what}: a delayed old-owner call"),
        )
        .await??;
    expect_equal(
        &delayed.messages.first().map(|m| m.is_error),
        &Some(true),
        &format!("{what}: the old owner's delayed call is refused"),
    )?;
    rig.advance(200).await;
    expect_equal(
        &rig.bodies(),
        &if point.new_call_runs {
            vec!["s:c0".to_string(), "s:c1".to_string()]
        } else {
            vec!["s:c0".to_string()]
        },
        &format!("{what}: tool body runs"),
    )?;
    let states: Vec<(String, String)> = rig
        .rows()
        .await?
        .into_iter()
        .map(|row| (row.command_id, row.progress.state().to_string()))
        .collect();
    expect_equal(
        &states,
        &vec![
            ("brain-new-1".to_string(), "acked".to_string()),
            ("brain-old-1".to_string(), "acked".to_string()),
        ],
        &format!("{what}: one durable record per call that reached the ledger, acknowledged"),
    )?;
    Ok(())
}

pub fn kill_epoch_declarations() -> Vec<KillEpochDeclaration> {
    points()
        .into_iter()
        .map(|p| KillEpochDeclaration {
            id: p.id,
            expected: p.expected,
        })
        .collect()
}

pub fn kill_epoch_cases() -> BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> = BTreeMap::new();
    for p in points() {
        let id = p.id;
        map.insert(
            id,
            Arc::new(move |adapters| {
                let p_c = p.clone();
                Box::pin(run_kill_epoch_case(adapters, p_c))
            }),
        );
    }
    map
}

pub const KILL_EPOCH_DECLARATIONS: fn() -> Vec<KillEpochDeclaration> = kill_epoch_declarations;
pub const KILL_EPOCH_CASES: fn() -> BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> =
    kill_epoch_cases;
