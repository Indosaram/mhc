//! Port of senpi `packages/agent/src/execution/testing/kill-hand.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::types::ContentBlock;
use maho_ai::utils::tool_pair_repair::TOOL_RESULT_PLACEHOLDER;
use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::remote_tool_host::HandPlacement;
use crate::types::ToolExecutionMode;

use super::hand_harness::{
    HandCaseAdapters, KillAt, KillPoint, OpenHandRigOptions, call, open_hand_rig,
};

const COMMITS: &[&str] = &[
    "admit", "prepare", "effect", "freeze", "release", "dispatch", "settle", "ack",
];
const PLACEMENTS: &[HandPlacement] = &[HandPlacement::SplitLocal, HandPlacement::CrossMachine];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Expected {
    Result,
    Restarted { uncertain: bool },
}

fn expected_after(last_durable: isize) -> Expected {
    if last_durable < 0 {
        return Expected::Result;
    }
    if last_durable <= 2 {
        return Expected::Restarted {
            uncertain: last_durable >= 2,
        };
    }
    if last_durable <= 4 {
        return Expected::Result;
    }
    if last_durable == 5 {
        return Expected::Restarted { uncertain: true };
    }
    Expected::Result
}

fn body_runs(kill: KillPoint, last_durable: isize) -> usize {
    if (0..=2).contains(&last_durable) {
        return 0;
    }
    if last_durable == 5 {
        return usize::from(kill.commit == 6);
    }
    1
}

#[derive(Debug, Clone)]
pub struct KillHandDeclaration {
    pub id: &'static str,
    pub operation: &'static str,
    pub at: KillAt,
    pub placement: HandPlacement,
    pub expected: &'static str,
}

fn placement_label(placement: HandPlacement) -> &'static str {
    match placement {
        HandPlacement::SplitLocal => "split-local",
        HandPlacement::CrossMachine => "cross-machine",
    }
}

fn describe_expected(placement: HandPlacement, expected: Expected, runs: usize) -> String {
    let body = if runs == 0 {
        "the tool never runs"
    } else {
        "the tool runs exactly once"
    };
    match expected {
        Expected::Result => format!("the session gets the tool's result; {body}"),
        Expected::Restarted { uncertain } => {
            if placement == HandPlacement::SplitLocal {
                format!("the session gets today's placeholder result, no uncertain outcome; {body}")
            } else {
                format!(
                    "the session gets hand_restarted, {}, never rerun; {body}",
                    if uncertain {
                        "uncertain"
                    } else {
                        "not uncertain"
                    }
                )
            }
        }
    }
}

async fn run_kill_hand_case(
    adapters: HandCaseAdapters,
    kill: KillPoint,
    placement: HandPlacement,
    what: String,
    expected: Expected,
    runs: usize,
) -> ConformanceResult {
    let rig = open_hand_rig(
        adapters,
        OpenHandRigOptions {
            placement,
            kill_at: Some(kill),
            fault: None,
        },
    )
    .await?;
    let outcome_fut = rig.execute("scope", vec![call("c1")], ToolExecutionMode::Parallel);
    let killed_fut = rig.wait_killed();
    let outcome_handle = tokio::spawn(outcome_fut);
    killed_fut.await?;
    rig.restart().await?;
    let host_res = rig
        .settle(
            async move { outcome_handle.await.map_err(|e| e.to_string())? },
            &format!("{what}: the session got the call's outcome"),
        )
        .await??;
    rig.advance(200).await;

    let message = host_res.messages.first();
    let text = message
        .map(|m| {
            m.content
                .iter()
                .filter_map(|p| match p {
                    ContentBlock::Text(t) => Some(t.text.as_str()),
                    _ => None,
                })
                .collect::<String>()
        })
        .unwrap_or_default();
    let is_error = message.map(|m| m.is_error);

    match expected {
        Expected::Result => {
            expect_equal(
                &json!([text, is_error]),
                &json!(["ran:c1", false]),
                &format!("{what}: the tool's result"),
            )?;
        }
        Expected::Restarted { uncertain } => {
            if placement == HandPlacement::SplitLocal {
                expect_equal(
                    &json!([text, is_error]),
                    &json!([TOOL_RESULT_PLACEHOLDER, false]),
                    &format!("{what}: today's placeholder"),
                )?;
            } else {
                let details = message.and_then(|m| m.details.clone());
                expect_equal(
                    &details,
                    &Some(json!({
                        "outcome": "hand_restarted",
                        "uncertain": uncertain,
                        "hooksRan": uncertain,
                    })),
                    &format!("{what}: the endpoint outcome"),
                )?;
                expect_equal(
                    &is_error,
                    &Some(true),
                    &format!("{what}: reported as an error"),
                )?;
            }
        }
    }
    expect_equal(
        &rig.bodies().len(),
        &runs,
        &format!("{what}: tool body runs"),
    )?;
    let states: Vec<&str> = rig
        .rows()
        .await?
        .iter()
        .map(|r| r.progress.state())
        .collect();
    expect_equal(
        &states,
        &vec!["acked"],
        &format!("{what}: one durable record, acknowledged"),
    )?;
    Ok(())
}

pub fn kill_hand_declarations() -> Vec<KillHandDeclaration> {
    let mut out = Vec::new();
    for (commit, &operation) in COMMITS.iter().enumerate() {
        for &at in &[KillAt::Before, KillAt::Durable] {
            for &placement in PLACEMENTS {
                let kill = KillPoint { commit, at };
                let last_durable = if at == KillAt::Durable {
                    commit as isize
                } else {
                    commit as isize - 1
                };
                let expected = expected_after(last_durable);
                let runs = body_runs(kill, last_durable);
                let id: &'static str = Box::leak(
                    format!(
                        "KILL-HAND-{:02}-{}-{}",
                        commit + 1,
                        if at == KillAt::Before { "B" } else { "D" },
                        if placement == HandPlacement::SplitLocal {
                            "SL"
                        } else {
                            "XM"
                        }
                    )
                    .into_boxed_str(),
                );
                let expected_str: &'static str = Box::leak(
                    format!(
                        "{}: kill {} the {operation} commit; {}; one durable record",
                        placement_label(placement),
                        at.as_str(),
                        describe_expected(placement, expected, runs)
                    )
                    .into_boxed_str(),
                );
                out.push(KillHandDeclaration {
                    id,
                    operation,
                    at,
                    placement,
                    expected: expected_str,
                });
            }
        }
    }
    out
}

pub fn kill_hand_cases() -> BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> = BTreeMap::new();
    for (commit, &operation) in COMMITS.iter().enumerate() {
        for &at in &[KillAt::Before, KillAt::Durable] {
            for &placement in PLACEMENTS {
                let kill = KillPoint { commit, at };
                let last_durable = if at == KillAt::Durable {
                    commit as isize
                } else {
                    commit as isize - 1
                };
                let expected = expected_after(last_durable);
                let runs = body_runs(kill, last_durable);
                let id: &'static str = Box::leak(
                    format!(
                        "KILL-HAND-{:02}-{}-{}",
                        commit + 1,
                        if at == KillAt::Before { "B" } else { "D" },
                        if placement == HandPlacement::SplitLocal {
                            "SL"
                        } else {
                            "XM"
                        }
                    )
                    .into_boxed_str(),
                );
                let what = format!(
                    "{id} ({operation} {}, {})",
                    at.as_str(),
                    placement_label(placement)
                );
                map.insert(
                    id,
                    Arc::new(move |adapters| {
                        let w = what.clone();
                        Box::pin(run_kill_hand_case(
                            adapters, kill, placement, w, expected, runs,
                        ))
                    }),
                );
            }
        }
    }
    map
}

pub const KILL_HAND_DECLARATIONS: fn() -> Vec<KillHandDeclaration> = kill_hand_declarations;
pub const KILL_HAND_CASES: fn() -> BTreeMap<&'static str, CaseImplementation<HandCaseAdapters>> =
    kill_hand_cases;
