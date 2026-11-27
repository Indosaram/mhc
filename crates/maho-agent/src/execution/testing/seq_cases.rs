//! Port of senpi `packages/agent/src/execution/testing/seq-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::coordinator_sequential::create_sequential_coordinator;
use crate::execution::coordinator_types::{
    BatchDecision, BatchResult, CallOutcome, NotRunCause, OutcomeInterruptionReason,
};

use super::batch_executor::{BatchCaseAdapters, PausePoint};
use super::batch_harness::{
    BrainKill, BrainKillAt, batch_harness, batch_plan, call_spec, expect_rejects_msg, result_of,
};

type SeqCase = CaseImplementation<BatchCaseAdapters>;

fn steps(id: &str) -> Vec<String> {
    vec![
        format!("shim:{id}"),
        format!("hook:{id}"),
        format!("body:{id}"),
        format!("done:{id}"),
    ]
}

async fn interleave(adapters: BatchCaseAdapters) -> ConformanceResult {
    let not_run = CallOutcome::NotRun {
        cause: NotRunCause::BatchAbort,
    };

    let h = batch_harness(adapters.clone()).await?;
    let plan = batch_plan(
        "seq-01",
        &[
            call_spec("a", "e1"),
            call_spec("b", "e2"),
            ("x", "e1", "deny", false),
            call_spec("c", "e1"),
        ],
    );
    let sequential = create_sequential_coordinator(h.brain(None, None, None).await?);
    let result = sequential.run(plan.clone()).await?;
    let blocked = CallOutcome::Blocked {
        result: json!("blocked: deny"),
    };
    expect_equal(
        &result.outcomes,
        &vec![
            result_of("a", "read"),
            result_of("b", "read"),
            blocked,
            result_of("c", "read"),
        ],
        "every call settles",
    )?;
    let mut expected_trace = steps("a");
    expected_trace.extend(steps("b"));
    expected_trace.push("shim:x".to_string());
    expected_trace.push("hook:x".to_string());
    expected_trace.extend(steps("c"));
    expect_equal(
        &h.trace(),
        &expected_trace,
        "prepare and run interleave per call, a blocked call does not stop the next",
    )?;
    expect_equal(
        &sequential.run(plan).await?,
        &result,
        "a finished run returns its stored outcomes",
    )?;
    expect_equal(&h.trace().len(), &14usize, "and runs nothing again")?;

    let crash = batch_harness(adapters.clone()).await?;
    let crash_plan = batch_plan(
        "seq-01-crash",
        &[
            call_spec("a", "e1"),
            call_spec("b", "e2"),
            call_spec("c", "e1"),
        ],
    );
    let dying = create_sequential_coordinator(
        crash
            .brain(
                Some(BrainKill {
                    label: "release:b".to_string(),
                    at: BrainKillAt::After,
                }),
                None,
                None,
            )
            .await?,
    );
    expect_rejects_msg(dying.run(crash_plan.clone()), "brain killed around", "kill").await?;
    let mut crash_trace_mid = steps("a");
    crash_trace_mid.push("shim:b".to_string());
    crash_trace_mid.push("hook:b".to_string());
    expect_equal(
        &crash.trace(),
        &crash_trace_mid,
        "the crashed brain never prepared c",
    )?;
    let resumed = create_sequential_coordinator(crash.brain(None, None, None).await?)
        .run(crash_plan)
        .await?;
    expect_equal(
        &resumed.outcomes,
        &vec![
            result_of("a", "read"),
            result_of("b", "read"),
            result_of("c", "read"),
        ],
        "recovery reaches every outcome",
    )?;
    let mut full_crash_trace = steps("a");
    full_crash_trace.extend(steps("b"));
    full_crash_trace.extend(steps("c"));
    expect_equal(
        &crash.trace(),
        &full_crash_trace,
        "each step once, in call order",
    )?;

    let stop = batch_harness(adapters.clone()).await?;
    let stop_plan = batch_plan(
        "seq-01-stop",
        &[
            call_spec("a", "e1"),
            ("s", "e2", "stop", false),
            call_spec("c", "e1"),
        ],
    );
    let stopped = create_sequential_coordinator(stop.brain(None, None, None).await?)
        .run(stop_plan.clone())
        .await?;
    expect_equal(
        &stopped,
        &BatchResult {
            decision: BatchDecision::Abort,
            outcomes: vec![result_of("a", "read"), not_run.clone(), not_run],
        },
        "a stop ends the run",
    )?;
    let mut expected_stop_trace = steps("a");
    expected_stop_trace.push("shim:s".to_string());
    expected_stop_trace.push("hook:s".to_string());
    expect_equal(
        &stop.trace(),
        &expected_stop_trace,
        "the call after a stop is never prepared",
    )?;
    let replayed = create_sequential_coordinator(stop.brain(None, None, None).await?)
        .run(stop_plan)
        .await?;
    expect_equal(
        &replayed,
        &stopped,
        "recovery after a stop prepares nothing new",
    )?;
    expect_equal(&stop.trace().len(), &6usize, "and runs nothing again")?;

    let restart = batch_harness(adapters).await?;
    let restart_plan = batch_plan(
        "seq-01-restart",
        &[call_spec("r", "e1"), call_spec("c", "e2")],
    );
    let hook_r = restart.hold("r", PausePoint::Hook);
    let brain = create_sequential_coordinator(restart.brain(None, None, None).await?);
    let brain_c = Arc::clone(&brain);
    let plan_c = restart_plan.clone();
    let first = tokio::spawn(async move { brain_c.run(plan_c).await });
    hook_r.reached().await;
    restart.restart_executor("e1").await?;
    expect_rejects_msg(
        async { first.await.map_err(|e| e.to_string())? },
        "was killed",
        "the executor died during r's preparation",
    )
    .await?;
    let interrupted = brain.run(restart_plan).await?;
    expect_equal(
        &interrupted.outcomes,
        &vec![
            CallOutcome::Interrupted {
                reason: OutcomeInterruptionReason::HandRestarted,
                uncertain: true,
            },
            CallOutcome::NotRun {
                cause: NotRunCause::InterruptedPreparation,
            },
        ],
        "the call after an interrupted preparation replays with today's placeholder, as in one batch (X12)",
    )?;
    expect_equal(&restart.count("shim:c"), &0usize, "and is never prepared")?;
    Ok(())
}

pub fn seq_cases() -> BTreeMap<&'static str, SeqCase> {
    let mut map: BTreeMap<&'static str, SeqCase> = BTreeMap::new();
    map.insert("SEQ-01", Arc::new(|a| Box::pin(interleave(a))));
    map
}

pub const SEQ_CASES: fn() -> BTreeMap<&'static str, SeqCase> = seq_cases;
