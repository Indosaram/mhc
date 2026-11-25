//! Port of senpi `packages/agent/src/execution/testing/kill-batch.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::coordinator_types::{BatchDecision, BatchResult, CallOutcome, NotRunCause};

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::{
    BrainKill, BrainKillAt, CallSpec, batch_harness, batch_plan, call_spec, expect_rejects_msg,
    result_of,
};

#[derive(Clone)]
struct Scenario {
    name: &'static str,
    calls: Vec<CallSpec>,
    writes: Vec<&'static str>,
    expected: BatchResult,
    prepared: Vec<&'static str>,
    ran: Vec<&'static str>,
}

fn scenarios() -> Vec<Scenario> {
    let not_run = CallOutcome::NotRun {
        cause: NotRunCause::BatchAbort,
    };
    vec![
        Scenario {
            name: "a released wave then a sequential call",
            calls: vec![
                call_spec("a", "e1"),
                call_spec("b", "e2"),
                ("c", "e1", "read", true),
            ],
            writes: vec![
                "manifest",
                "prepare:a",
                "prepare:b",
                "prepare:c",
                "decision",
                "release:a",
                "release:b",
                "outcome:a",
                "outcome:b",
                "release:c",
                "outcome:c",
            ],
            expected: BatchResult {
                decision: BatchDecision::Release,
                outcomes: vec![
                    result_of("a", "read"),
                    result_of("b", "read"),
                    result_of("c", "read"),
                ],
            },
            prepared: vec!["a", "b", "c"],
            ran: vec!["a", "b", "c"],
        },
        Scenario {
            name: "a batch_abort reply",
            calls: vec![
                call_spec("a", "e1"),
                ("b", "e2", "stop", false),
                call_spec("c", "e1"),
            ],
            writes: vec![
                "manifest",
                "prepare:a",
                "prepare:b",
                "decision",
                "outcome:a",
                "outcome:b",
                "outcome:c",
            ],
            expected: BatchResult {
                decision: BatchDecision::Abort,
                outcomes: vec![not_run.clone(), not_run.clone(), not_run],
            },
            prepared: vec!["a", "b"],
            ran: vec![],
        },
        Scenario {
            name: "a blocked call beside a released call (the executor's blocked reply must be durable)",
            calls: vec![("a", "e1", "deny", false), call_spec("b", "e2")],
            writes: vec![
                "manifest",
                "prepare:a",
                "prepare:b",
                "decision",
                "outcome:a",
                "release:b",
                "outcome:b",
            ],
            expected: BatchResult {
                decision: BatchDecision::Release,
                outcomes: vec![
                    CallOutcome::Blocked {
                        result: json!("blocked: deny"),
                    },
                    result_of("b", "read"),
                ],
            },
            prepared: vec!["a", "b"],
            ran: vec!["b"],
        },
    ]
}

const AT: &[BrainKillAt] = &[BrainKillAt::Before, BrainKillAt::After];

#[derive(Debug, Clone)]
pub struct KillBatchDeclaration {
    pub id: &'static str,
    pub scenario: &'static str,
    pub kill: BrainKill,
    pub placement: &'static str,
}

async fn run_kill_batch_case(
    adapters: BatchCaseAdapters,
    id: &'static str,
    scenario: Scenario,
    kill: BrainKill,
) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan(&id.to_lowercase(), &scenario.calls);
    let what = format!("{} {}", kill.at.as_str(), kill.label);
    expect_rejects_msg(
        h.brain(Some(kill), None, None).await?.run(plan.clone()),
        "brain killed around",
        &what,
    )
    .await?;
    expect_equal(
        &h.brain(None, None, None).await?.run(plan).await?,
        &scenario.expected,
        "recovery reaches the uncrashed outcomes",
    )?;
    for (execution_id, executor, _, _) in &scenario.calls {
        let prepared_once = usize::from(scenario.prepared.contains(execution_id));
        for step in ["shim", "hook"] {
            expect_equal(
                &h.count(&format!("{step}:{execution_id}")),
                &prepared_once,
                &format!("{step}:{execution_id} once at most"),
            )?;
        }
        let ran_once = usize::from(scenario.ran.contains(execution_id));
        expect_equal(
            &h.count(&format!("body:{execution_id}")),
            &ran_once,
            &format!("body:{execution_id}"),
        )?;
        if prepared_once == 1 {
            expect_equal(
                &h.ledger_state(executor, execution_id).await?,
                &Some("settled".to_string()),
                "stable ids",
            )?;
        }
    }
    Ok(())
}

pub fn kill_batch_declarations() -> Vec<KillBatchDeclaration> {
    let all = scenarios();
    let mut out = Vec::new();
    let mut base = 0usize;
    for scenario in &all {
        for (write_index, &label) in scenario.writes.iter().enumerate() {
            for (at_index, &at) in AT.iter().enumerate() {
                let number = base + write_index + 1;
                let id: &'static str =
                    Box::leak(format!("KILL-BATCH-{number:02}-{}", at_index + 1).into_boxed_str());
                out.push(KillBatchDeclaration {
                    id,
                    scenario: scenario.name,
                    kill: BrainKill {
                        label: label.to_string(),
                        at,
                    },
                    placement: "everywhere",
                });
            }
        }
        base += scenario.writes.len();
    }
    out
}

pub fn kill_batch_cases() -> BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> {
    let all = scenarios();
    let mut map: BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> = BTreeMap::new();
    let mut base = 0usize;
    for scenario in &all {
        for (write_index, &label) in scenario.writes.iter().enumerate() {
            for (at_index, &at) in AT.iter().enumerate() {
                let number = base + write_index + 1;
                let id: &'static str =
                    Box::leak(format!("KILL-BATCH-{number:02}-{}", at_index + 1).into_boxed_str());
                let sc = scenario.clone();
                let kill = BrainKill {
                    label: label.to_string(),
                    at,
                };
                map.insert(
                    id,
                    Arc::new(move |adapters| {
                        let sc_c = sc.clone();
                        let kill_c = kill.clone();
                        Box::pin(run_kill_batch_case(adapters, id, sc_c, kill_c))
                    }),
                );
            }
        }
        base += scenario.writes.len();
    }
    map
}

pub const KILL_BATCH_DECLARATIONS: fn() -> Vec<KillBatchDeclaration> = kill_batch_declarations;
pub const KILL_BATCH_CASES: fn() -> BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> =
    kill_batch_cases;
