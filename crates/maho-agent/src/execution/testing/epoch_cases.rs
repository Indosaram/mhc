//! Port of senpi `packages/agent/src/execution/testing/epoch-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::join_all;
use maho_ai::types::ContentBlock;
use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::DurableStoreOptions;
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::tool_host::ToolHostResult;

use super::epoch_harness::{
    EpochBrainSpec, OpenEpochRigOptions, floor_commit, open_epoch_rig, step_commit,
};
use super::hand_harness::{HandCaseAdapters, call};

type EpochCase = CaseImplementation<HandCaseAdapters>;

pub fn texts(res: &ToolHostResult) -> Vec<String> {
    res.messages
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

async fn floor_ack_barrier(adapters: HandCaseAdapters) -> ConformanceResult {
    let old = old_brain();
    let new = new_brain();
    let rig = open_epoch_rig(
        adapters,
        OpenEpochRigOptions {
            brains: vec![old.clone(), new.clone()],
            placement: None,
            kill: None,
        },
    )
    .await?;
    rig.set_owner("s", &old.name);
    let before = rig
        .settle(
            rig.execute(&old.name, "s", vec![call("c0")]),
            "EPOCH-01: the first owner's call",
        )
        .await??;
    expect_equal(
        &texts(&before),
        &vec!["ran:c0".to_string()],
        "EPOCH-01: the first owner runs on its floor",
    )?;
    rig.set_owner("s", &new.name);
    let barrier = Arc::new(rig.hold(floor_commit("s", new.epoch)));
    let moved_handle = tokio::spawn(rig.execute(&new.name, "s", vec![call("c1")]));
    let barrier_c = Arc::clone(&barrier);
    rig.settle(
        async move { barrier_c.wait_reached().await },
        "EPOCH-01: the new owner's floor reached the hand's store",
    )
    .await?;
    rig.advance(2_000).await;
    expect_equal(
        &rig.bodies(),
        &vec!["s:c0".to_string()],
        "EPOCH-01: nothing of the new owner's call ran while its floor ACK was held",
    )?;
    expect_equal(
        &rig.rows().await?.len(),
        &1usize,
        "EPOCH-01: the new owner's call was not even recorded",
    )?;
    barrier.release();
    let moved = rig
        .settle(
            async move { moved_handle.await.map_err(|e| e.to_string())? },
            "EPOCH-01: the new owner's call",
        )
        .await??;
    expect_equal(
        &texts(&moved),
        &vec!["ran:c1".to_string()],
        "EPOCH-01: result",
    )?;
    expect_equal(
        &rig.bodies(),
        &vec!["s:c0".to_string(), "s:c1".to_string()],
        "EPOCH-01: the new owner's call ran exactly once after the ACK",
    )?;
    Ok(())
}

async fn concurrent_claims(adapters: HandCaseAdapters) -> ConformanceResult {
    let first = EpochBrainSpec {
        name: "brain-first".to_string(),
        epoch: 1,
    };
    let rivals = vec![
        EpochBrainSpec {
            name: "brain-a".to_string(),
            epoch: 2,
        },
        EpochBrainSpec {
            name: "brain-b".to_string(),
            epoch: 2,
        },
    ];
    let mut brains = vec![first.clone()];
    brains.extend(rivals.clone());
    let rig = open_epoch_rig(
        adapters.clone(),
        OpenEpochRigOptions {
            brains,
            placement: None,
            kill: None,
        },
    )
    .await?;
    rig.set_owner("s", &first.name);
    rig.settle(
        rig.execute(&first.name, "s", vec![call("c0")]),
        "EPOCH-02: the first owner's call",
    )
    .await??;

    let session = adapters
        .store
        .create_medium()
        .await
        .map_err(|e| e.to_string())?;
    let mut opened = Vec::new();
    for _ in 0..3 {
        opened.push(
            adapters
                .store
                .open(&session, DurableStoreOptions::default())
                .await
                .map_err(|e| e.to_string())?,
        );
    }
    opened[0].claim(0, 1).await.map_err(|e| e.to_string())?;
    let claims = join_all([opened[1].claim(1, 2), opened[2].claim(1, 2)]).await;
    let won: Vec<&EpochBrainSpec> = rivals
        .iter()
        .enumerate()
        .filter(|(idx, _)| claims[*idx].is_ok())
        .map(|(_, b)| b)
        .collect();
    expect_equal(
        &won.len(),
        &1usize,
        "EPOCH-02: exactly one of two concurrent CAS claims wins",
    )?;
    let winner = won.first().copied().unwrap_or(&rivals[0]);
    let loser = rivals
        .iter()
        .find(|r| r.name != winner.name)
        .unwrap_or(&rivals[1]);
    rig.set_owner("s", &winner.name);
    let (ran_res, refused_res) = rig
        .settle(
            {
                let f1 = rig.execute(&winner.name, "s", vec![call("cw")]);
                let f2 = rig.execute(&loser.name, "s", vec![call("cl")]);
                async move { tokio::join!(f1, f2) }
            },
            "EPOCH-02: both claimants' calls",
        )
        .await?;
    let ran = ran_res?;
    let refused = refused_res?;
    expect_equal(
        &texts(&ran),
        &vec!["ran:cw".to_string()],
        "EPOCH-02: the winner's call runs",
    )?;
    expect_equal(
        &json!([
            texts(&refused),
            refused.messages.first().map(|m| m.is_error),
        ]),
        &json!([
            ["The hand refused this session's epoch: peer_unauthorized"],
            true
        ]),
        "EPOCH-02: the loser's floor is refused and its call is never sent",
    )?;
    expect_equal(
        &rig.bodies(),
        &vec!["s:c0".to_string(), "s:cw".to_string()],
        "EPOCH-02: only the winner's call ran",
    )?;
    Ok(())
}

async fn delayed_old_owner(adapters: HandCaseAdapters) -> ConformanceResult {
    let old = old_brain();
    let new = new_brain();
    let rig = open_epoch_rig(
        adapters,
        OpenEpochRigOptions {
            brains: vec![old.clone(), new.clone()],
            placement: None,
            kill: None,
        },
    )
    .await?;
    rig.set_owner("s", &old.name);
    rig.settle(
        rig.execute(&old.name, "s", vec![call("c0")]),
        "EPOCH-03: the old owner's first call",
    )
    .await??;
    let prepare = Arc::new(rig.hold(step_commit(&old.name, "prepare")));
    let delayed_handle = tokio::spawn(rig.execute(&old.name, "s", vec![call("c2")]));
    let prepare_c = Arc::clone(&prepare);
    rig.settle(
        async move { prepare_c.wait_reached().await },
        "EPOCH-03: the old owner's delayed call is mid-walk",
    )
    .await?;
    rig.set_owner("s", &new.name);
    let moved_handle = tokio::spawn(rig.execute(&new.name, "s", vec![call("c1")]));
    rig.advance(2_000).await;
    prepare.release();
    let moved = rig
        .settle(
            async move { moved_handle.await.map_err(|e| e.to_string())? },
            "EPOCH-03: the new owner's call",
        )
        .await??;
    expect_equal(
        &texts(&moved),
        &vec!["ran:c1".to_string()],
        "EPOCH-03: new owner",
    )?;
    let delayed = rig
        .settle(
            async move { delayed_handle.await.map_err(|e| e.to_string())? },
            "EPOCH-03: the old owner's mid-walk call",
        )
        .await??;
    expect_equal(
        &texts(&delayed),
        &vec!["The hand refused this tool call: epoch_fenced".to_string()],
        "EPOCH-03: the old owner's walk is refused at its next step",
    )?;
    let late = rig
        .settle(
            rig.execute(&old.name, "s", vec![call("c3")]),
            "EPOCH-03: a delayed old-owner invoke",
        )
        .await??;
    expect_equal(
        &texts(&late),
        &vec!["The hand refused this tool call: epoch_fenced".to_string()],
        "EPOCH-03: a delayed invoke is fenced",
    )?;
    rig.advance(200).await;
    let rows = rig.rows().await?;
    expect_equal(
        &rig.bodies(),
        &vec!["s:c0".to_string(), "s:c1".to_string()],
        "EPOCH-03: no old-owner call ran after the move",
    )?;
    let summary: Vec<(String, String)> = rows
        .into_iter()
        .map(|row| {
            let state = match &row.progress {
                crate::execution::ledger_types::ExecutionProgress::Settled { outcome } => {
                    outcome.reason_str().to_string()
                }
                other => other.state().to_string(),
            };
            (row.command_id, state)
        })
        .collect();
    expect_equal(
        &summary,
        &vec![
            ("brain-new-1".to_string(), "acked".to_string()),
            ("brain-old-1".to_string(), "acked".to_string()),
            ("brain-old-3".to_string(), "cancelled".to_string()),
        ],
        "EPOCH-03: the fenced walk's record is cancelled, never dispatched; the delayed invoke left no record",
    )?;
    Ok(())
}

pub fn epoch_cases() -> BTreeMap<&'static str, EpochCase> {
    let mut map: BTreeMap<&'static str, EpochCase> = BTreeMap::new();
    map.insert("EPOCH-01", Arc::new(|a| Box::pin(floor_ack_barrier(a))));
    map.insert("EPOCH-02", Arc::new(|a| Box::pin(concurrent_claims(a))));
    map.insert("EPOCH-03", Arc::new(|a| Box::pin(delayed_old_owner(a))));
    map
}

pub const EPOCH_CASES: fn() -> BTreeMap<&'static str, EpochCase> = epoch_cases;
