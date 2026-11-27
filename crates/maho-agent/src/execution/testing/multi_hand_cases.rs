//! Port of senpi `packages/agent/src/execution/testing/multi-hand-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use futures::future::join_all;
use maho_ai::types::{ContentBlock, ToolResultMessage};
use serde_json::{Value, json};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::remote_tool_host::HandPlacement;
use crate::types::ToolExecutionMode;

use super::hand_harness::{
    HandCaseAdapters, KillAt, KillPoint, OpenHandRigOptions, call, open_hand_rig,
};

type HandCase = CaseImplementation<HandCaseAdapters>;

const SCOPES: &[&str] = &["a", "b", "c"];
const CALLS_PER_SCOPE: usize = 3;

fn call_ids(scope: &str) -> Vec<String> {
    (1..=CALLS_PER_SCOPE)
        .map(|i| format!("{scope}{i}"))
        .collect()
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

async fn many_scopes_on_one_hand(adapters: HandCaseAdapters) -> ConformanceResult {
    let rig = open_hand_rig(
        adapters,
        OpenHandRigOptions {
            placement: HandPlacement::SplitLocal,
            kill_at: None,
            fault: None,
        },
    )
    .await?;
    rig.gate_bodies();
    let batches: Vec<_> = SCOPES
        .iter()
        .map(|scope| {
            let calls: Vec<_> = call_ids(scope).iter().map(|id| call(id)).collect();
            tokio::spawn(rig.execute(scope, calls, ToolExecutionMode::Parallel))
        })
        .collect();
    rig.run_until(
        || rig.bodies().len() == SCOPES.len() * CALLS_PER_SCOPE,
        "MULTI-HAND-01: every body started",
    )
    .await?;
    for scope in SCOPES {
        let actual: Vec<String> = rig
            .bodies()
            .into_iter()
            .filter(|b| b.starts_with(&format!("{scope}:")))
            .collect();
        let expected: Vec<String> = call_ids(scope)
            .into_iter()
            .map(|id| format!("{scope}:{id}"))
            .collect();
        expect_equal(
            &actual,
            &expected,
            &format!("MULTI-HAND-01: scope {scope} kept its lane order"),
        )?;
    }
    for &scope in SCOPES.iter().rev() {
        for id in call_ids(scope).into_iter().rev() {
            rig.release(&id);
        }
    }
    let settled = rig
        .settle(join_all(batches), "MULTI-HAND-01: every batch settled")
        .await?;
    let mut actual_texts = Vec::new();
    for res in settled {
        let host_res = res.map_err(|e| e.to_string())??;
        actual_texts.push(texts(&host_res.messages));
    }
    let expected_texts: Vec<Vec<String>> = SCOPES
        .iter()
        .map(|scope| {
            call_ids(scope)
                .into_iter()
                .map(|id| format!("ran:{id}"))
                .collect()
        })
        .collect();
    expect_equal(
        &actual_texts,
        &expected_texts,
        "MULTI-HAND-01: each scope's results in its source order",
    )?;
    Ok(())
}

async fn kill_with_scopes_in_flight(adapters: HandCaseAdapters) -> ConformanceResult {
    let dispatch_commits = SCOPES.len() * 6;
    let rig = open_hand_rig(
        adapters,
        OpenHandRigOptions {
            placement: HandPlacement::CrossMachine,
            kill_at: Some(KillPoint {
                commit: dispatch_commits,
                at: KillAt::Before,
            }),
            fault: None,
        },
    )
    .await?;
    rig.gate_bodies();
    let batches: Vec<_> = SCOPES
        .iter()
        .map(|scope| {
            let calls = vec![call(&format!("{scope}1"))];
            tokio::spawn(rig.execute(scope, calls, ToolExecutionMode::Parallel))
        })
        .collect();
    rig.run_until(
        || rig.bodies().len() == SCOPES.len(),
        "MULTI-KILL-01: every scope's call is running",
    )
    .await?;
    rig.release("a1");
    rig.wait_killed().await?;
    rig.restart().await?;
    let settled = rig
        .settle(
            join_all(batches),
            "MULTI-KILL-01: every scope got its outcome",
        )
        .await?;
    let mut actual_details: Vec<Vec<Option<Value>>> = Vec::new();
    for res in settled {
        let host_res = res.map_err(|e| e.to_string())??;
        actual_details.push(host_res.messages.into_iter().map(|m| m.details).collect());
    }
    let expected_details: Vec<Vec<Option<Value>>> = SCOPES
        .iter()
        .map(|_| {
            vec![Some(json!({
                "outcome": "hand_restarted",
                "uncertain": true,
                "hooksRan": true,
            }))]
        })
        .collect();
    expect_equal(
        &actual_details,
        &expected_details,
        "MULTI-KILL-01: each scope's call ended hand_restarted, uncertain",
    )?;
    let expected_bodies: Vec<String> = SCOPES.iter().map(|s| format!("{s}:{s}1")).collect();
    expect_equal(
        &rig.bodies(),
        &expected_bodies,
        "MULTI-KILL-01: no call ran twice",
    )?;
    rig.wait_for(
        || async {
            Ok(rig
                .rows()
                .await?
                .iter()
                .all(|r| r.progress.state() == "acked"))
        },
        "MULTI-KILL-01: every call's record acknowledged",
    )
    .await?;
    let states: Vec<&str> = rig
        .rows()
        .await?
        .iter()
        .map(|r| r.progress.state())
        .collect();
    let expected_states: Vec<&str> = SCOPES.iter().map(|_| "acked").collect();
    expect_equal(
        &states,
        &expected_states,
        "MULTI-KILL-01: one durable record per call, acknowledged",
    )?;
    Ok(())
}

pub fn multi_hand_cases() -> BTreeMap<&'static str, HandCase> {
    let mut map: BTreeMap<&'static str, HandCase> = BTreeMap::new();
    map.insert(
        "MULTI-HAND-01",
        Arc::new(|a| Box::pin(many_scopes_on_one_hand(a))),
    );
    map.insert(
        "MULTI-KILL-01",
        Arc::new(|a| Box::pin(kill_with_scopes_in_flight(a))),
    );
    map
}

pub const MULTI_HAND_CASES: fn() -> BTreeMap<&'static str, HandCase> = multi_hand_cases;
