//! Port of senpi `packages/agent/src/execution/testing/batch-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use maho_ai::utils::tool_pair_repair::TOOL_RESULT_PLACEHOLDER;
use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::coordinator_plan::interrupted_replay_text;
use crate::execution::coordinator_types::{
    BatchDecision, BatchResult, CallOutcome, NotRunCause, OutcomeInterruptionReason,
};

use super::batch_executor::BatchCaseAdapters;
use super::batch_executor::PausePoint;
use super::batch_harness::{
    BatchHarness, BrainKill, BrainKillAt, batch_harness, batch_plan, call_spec, expect_rejects_msg,
    result_of,
};

type BatchCase = CaseImplementation<BatchCaseAdapters>;

fn ran_once(h: &BatchHarness, ids: &[&str], steps: &[&str], what: &str) -> Result<(), String> {
    for id in ids {
        for step in steps {
            let key = format!("{step}:{id}");
            expect_equal(&h.count(&key), &1usize, &format!("{what}: {key}"))?;
        }
    }
    Ok(())
}

fn assert_before(h: &BatchHarness, first: &str, second: &str, what: &str) -> Result<(), String> {
    let trace = h.trace();
    let a = trace.iter().position(|s| s == first);
    let b = trace.iter().position(|s| s == second);
    match (a, b) {
        (Some(ai), Some(bi)) if ai <= bi => Ok(()),
        _ => Err(format!(
            "{what}: expected {first} before {second} in {}",
            trace.join(",")
        )),
    }
}

async fn crash_after_manifest(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan("batch-01", &[call_spec("a", "e1"), call_spec("b", "e2")]);
    expect_rejects_msg(
        h.brain(
            Some(BrainKill {
                label: "manifest".to_string(),
                at: BrainKillAt::After,
            }),
            None,
            None,
        )
        .await?
        .run(plan.clone()),
        "brain killed around",
        "kill",
    )
    .await?;
    expect_equal(
        &h.trace(),
        &Vec::<String>::new(),
        "nothing has run after the manifest alone",
    )?;
    let result = h.brain(None, None, None).await?.run(plan).await?;
    expect_equal(
        &result,
        &BatchResult {
            decision: BatchDecision::Release,
            outcomes: vec![result_of("a", "read"), result_of("b", "read")],
        },
        "re-prepared and run",
    )?;
    ran_once(
        &h,
        &["a", "b"],
        &["shim", "hook", "body"],
        "the same execution ids, each step once",
    )?;
    expect_equal(
        &h.ledger_state("e1", "a").await?,
        &Some("settled".to_string()),
        "the endpoint row carries the manifest's execution id",
    )?;
    Ok(())
}

async fn crash_after_prepares(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan("batch-02", &[call_spec("a", "e1"), call_spec("b", "e2")]);
    expect_rejects_msg(
        h.brain(
            Some(BrainKill {
                label: "decision".to_string(),
                at: BrainKillAt::Before,
            }),
            None,
            None,
        )
        .await?
        .run(plan.clone()),
        "brain killed around",
        "kill",
    )
    .await?;
    expect_equal(
        &h.trace(),
        &vec![
            "shim:a".to_string(),
            "hook:a".to_string(),
            "shim:b".to_string(),
            "hook:b".to_string(),
        ],
        "both prepared, no body before the decision",
    )?;
    let result = h.brain(None, None, None).await?.run(plan).await?;
    expect_equal(
        &result.outcomes,
        &vec![result_of("a", "read"), result_of("b", "read")],
        "the stored prepare replies decide RELEASE",
    )?;
    ran_once(
        &h,
        &["a", "b"],
        &["shim", "hook", "body"],
        "hooks never rerun",
    )?;
    Ok(())
}

async fn crash_inside_first_wave(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan(
        "batch-03",
        &[
            call_spec("a", "e1"),
            call_spec("b", "e2"),
            ("c", "e1", "read", true),
        ],
    );
    let body_a = h.hold("a", PausePoint::Body);
    expect_rejects_msg(
        h.brain(
            Some(BrainKill {
                label: "release:b".to_string(),
                at: BrainKillAt::Before,
            }),
            None,
            None,
        )
        .await?
        .run(plan.clone()),
        "brain killed around",
        "kill",
    )
    .await?;
    body_a.reached().await;
    let settled_b = h.written("outcome:b");
    let brain = h.brain(None, None, None).await?;
    let resumed = tokio::spawn(async move { brain.run(plan).await });
    let writes = settled_b.wait().await;
    expect_equal(
        &writes.iter().any(|w| w == "release:c"),
        &false,
        "the next wave is not released while wave 0 is unsettled",
    )?;
    body_a.open();
    let result = resumed.await.map_err(|e| e.to_string())??;
    expect_equal(
        &result.outcomes,
        &vec![
            result_of("a", "read"),
            result_of("b", "read"),
            result_of("c", "read"),
        ],
        "every call settles",
    )?;
    ran_once(
        &h,
        &["a", "b", "c"],
        &["hook", "body"],
        "a re-sent release never reruns a body",
    )?;
    assert_before(
        &h,
        "body:b",
        "done:a",
        "the unmarked wave-0 call is released beside the re-sent one",
    )?;
    assert_before(&h, "done:a", "body:c", "wave 1 waits for a")?;
    assert_before(&h, "done:b", "body:c", "wave 1 waits for b")?;
    Ok(())
}

async fn executor_restart_keeps_prepared(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan(
        "batch-05",
        &[call_spec("a", "e1"), ("b", "e1", "read", true)],
    );
    let dispatch_a = h.hold("a", PausePoint::Dispatch);
    let brain = h.brain(None, None, None).await?;
    let plan_c = plan.clone();
    let brain_c = Arc::clone(&brain);
    let first = tokio::spawn(async move { brain_c.run(plan_c).await });
    dispatch_a.reached().await;
    h.restart_executor("e1").await?;
    expect_rejects_msg(
        async { first.await.map_err(|e| e.to_string())? },
        "was killed",
        "the release in flight is lost with the executor",
    )
    .await?;
    expect_equal(
        &h.ledger_state("e1", "a").await?,
        &Some("released".to_string()),
        "released survives the restart",
    )?;
    expect_equal(
        &h.ledger_state("e1", "b").await?,
        &Some("prepared".to_string()),
        "prepared survives the restart",
    )?;
    dispatch_a.open();
    expect_equal(
        &brain.run(plan).await?.outcomes,
        &vec![result_of("a", "read"), result_of("b", "read")],
        "release proceeds",
    )?;
    ran_once(
        &h,
        &["a", "b"],
        &["hook", "body"],
        "nothing is prepared or run twice",
    )?;
    Ok(())
}

async fn lost_prepared_execution(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan("batch-06", &[call_spec("a", "e1"), call_spec("b", "e2")]);
    expect_rejects_msg(
        h.brain(
            Some(BrainKill {
                label: "decision".to_string(),
                at: BrainKillAt::After,
            }),
            None,
            None,
        )
        .await?
        .run(plan.clone()),
        "brain killed around",
        "kill",
    )
    .await?;
    h.lose_executor("e1").await?;
    let result = h.brain(None, None, None).await?.run(plan).await?;
    let lost = CallOutcome::Interrupted {
        reason: OutcomeInterruptionReason::UnknownExecution,
        uncertain: true,
    };
    expect_equal(
        &result.outcomes,
        &vec![lost.clone(), result_of("b", "read")],
        "a lost prepared execution is uncertain; the sibling settles",
    )?;
    expect_equal(&h.count("body:a"), &0usize, "the lost call never runs")?;
    expect_equal(
        &interrupted_replay_text(&lost),
        &TOOL_RESULT_PLACEHOLDER,
        "it replays with today's placeholder",
    )?;
    Ok(())
}

async fn waves_across_executors(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan(
        "batch-07",
        &[
            call_spec("a", "e1"),
            call_spec("b", "e2"),
            ("s", "e1", "read", true),
            call_spec("d", "e2"),
        ],
    );
    let hook_a = h.hold("a", PausePoint::Hook);
    let body_a = h.hold("a", PausePoint::Body);
    let body_b = h.hold("b", PausePoint::Body);
    let brain = h.brain(None, None, None).await?;
    let run = tokio::spawn(async move { brain.run(plan).await });
    hook_a.reached().await;
    expect_equal(
        &h.count("shim:b"),
        &0usize,
        "b is prepared only after a's prepare reply",
    )?;
    hook_a.open();
    tokio::join!(body_a.reached(), body_b.reached());
    let settled_b = h.written("outcome:b");
    body_b.open();
    let writes = settled_b.wait().await;
    expect_equal(
        &writes.iter().any(|w| w == "release:s"),
        &false,
        "the sequential call is not released while the wave runs",
    )?;
    body_a.open();
    let res = run.await.map_err(|e| e.to_string())??;
    expect_equal(
        &res.outcomes,
        &["a", "b", "s", "d"]
            .into_iter()
            .map(|id| result_of(id, "read"))
            .collect::<Vec<_>>(),
        "every call settles",
    )?;
    for (first, second) in [
        ("shim:a", "shim:b"),
        ("shim:b", "shim:s"),
        ("shim:s", "shim:d"),
    ] {
        assert_before(
            &h,
            first,
            second,
            "prepare runs in source order across executors",
        )?;
    }
    for (first, second) in [
        ("done:a", "body:s"),
        ("done:b", "body:s"),
        ("done:s", "body:d"),
    ] {
        assert_before(
            &h,
            first,
            second,
            "a release waits for its dependencies' final outcomes",
        )?;
    }

    let chain = batch_plan(
        "batch-07-blocked-link",
        &[
            ("s1", "e1", "read", true),
            ("s2", "e2", "deny", true),
            ("s3", "e1", "read", true),
            call_spec("p", "e2"),
        ],
    );
    let last = h.written("outcome:p");
    let blocked = CallOutcome::Blocked {
        result: json!("blocked: deny"),
    };
    let chain_res = h.brain(None, None, None).await?.run(chain).await?;
    expect_equal(
        &chain_res.outcomes,
        &vec![
            result_of("s1", "read"),
            blocked,
            result_of("s3", "read"),
            result_of("p", "read"),
        ],
        "a blocked link settles as blocked; the chain runs",
    )?;
    let order = last.wait().await;
    for (finished, released) in [("outcome:s1", "release:s3"), ("outcome:s3", "release:p")] {
        let a = order.iter().position(|w| w == finished);
        let b = order.iter().position(|w| w == released);
        match (a, b) {
            (Some(ai), Some(bi)) if ai <= bi => {}
            _ => {
                return Err(format!(
                    "a blocked link still waits on its own dependencies: {finished} before {released}"
                ));
            }
        }
    }
    Ok(())
}

async fn batch_abort(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan(
        "batch-08",
        &[
            call_spec("a", "e1"),
            ("b", "e2", "stop", false),
            call_spec("c", "e1"),
        ],
    );
    expect_rejects_msg(
        h.brain(
            Some(BrainKill {
                label: "decision".to_string(),
                at: BrainKillAt::After,
            }),
            None,
            None,
        )
        .await?
        .run(plan.clone()),
        "brain killed around",
        "kill",
    )
    .await?;
    let result = h.brain(None, None, None).await?.run(plan).await?;
    let not_run = CallOutcome::NotRun {
        cause: NotRunCause::BatchAbort,
    };
    expect_equal(
        &result,
        &BatchResult {
            decision: BatchDecision::Abort,
            outcomes: vec![not_run.clone(), not_run.clone(), not_run],
        },
        "ABORT ends every call",
    )?;
    expect_equal(
        &h.count("shim:c"),
        &0usize,
        "no call after the batch_abort reply is prepared",
    )?;
    expect_equal(
        &vec![h.count("body:a"), h.count("body:b")],
        &vec![0usize, 0usize],
        "no body runs",
    )?;
    expect_equal(
        &h.ledger_state("e1", "a").await?,
        &Some("settled".to_string()),
        "the re-sent abort ends the prepared call",
    )?;
    Ok(())
}

async fn blocked_beside_released(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let plan = batch_plan(
        "batch-09",
        &[
            ("a", "e1", "deny", false),
            call_spec("b", "e2"),
            ("c", "e1", "read", true),
        ],
    );
    let result = h.brain(None, None, None).await?.run(plan).await?;
    let blocked = CallOutcome::Blocked {
        result: json!("blocked: deny"),
    };
    expect_equal(
        &result,
        &BatchResult {
            decision: BatchDecision::Release,
            outcomes: vec![blocked, result_of("b", "read"), result_of("c", "read")],
        },
        "others run",
    )?;
    expect_equal(
        &h.count("body:a"),
        &0usize,
        "a blocked call's body never runs",
    )?;
    expect_equal(
        &h.ledger_state("e1", "a").await?,
        &Some("settled".to_string()),
        "the blocked call's endpoint is closed",
    )?;
    Ok(())
}

async fn replay_interrupted_preparation(
    adapters: BatchCaseAdapters,
    with_prepared_sibling: bool,
) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let ids: Vec<&'static str> = if with_prepared_sibling {
        vec!["a", "b"]
    } else {
        vec!["b"]
    };
    let specs: Vec<_> = ids.iter().map(|id| call_spec(id, "e1")).collect();
    let plan = batch_plan(&format!("replay-{}", ids.len()), &specs);
    let hook_b = h.hold("b", PausePoint::Hook);
    let brain = h.brain(None, None, None).await?;
    let plan_c = plan.clone();
    let brain_c = Arc::clone(&brain);
    let first = tokio::spawn(async move { brain_c.run(plan_c).await });
    hook_b.reached().await;
    h.restart_executor("e1").await?;
    expect_rejects_msg(
        async { first.await.map_err(|e| e.to_string())? },
        "was killed",
        "the executor died during preparation",
    )
    .await?;
    let res = brain.run(plan.clone()).await?;
    let interrupted = CallOutcome::Interrupted {
        reason: OutcomeInterruptionReason::HandRestarted,
        uncertain: true,
    };
    let sibling = CallOutcome::NotRun {
        cause: NotRunCause::InterruptedPreparation,
    };
    expect_equal(
        &res.decision,
        &BatchDecision::Abort,
        "unreleased calls are aborted",
    )?;
    let expected_outcomes = if with_prepared_sibling {
        vec![sibling, interrupted]
    } else {
        vec![interrupted]
    };
    expect_equal(&res.outcomes, &expected_outcomes, "outcomes")?;
    let texts: Vec<String> = res
        .outcomes
        .iter()
        .filter_map(interrupted_replay_text)
        .map(str::to_owned)
        .collect();
    let expected_texts: Vec<String> = ids
        .iter()
        .map(|_| TOOL_RESULT_PLACEHOLDER.to_string())
        .collect();
    expect_equal(&texts, &expected_texts, "today's placeholder")?;
    ran_once(&h, &ids, &["shim", "hook"], "preparation never runs again")?;
    let body_counts: Vec<usize> = ids
        .iter()
        .map(|id| h.count(&format!("body:{id}")))
        .collect();
    let zeros: Vec<usize> = ids.iter().map(|_| 0).collect();
    expect_equal(&body_counts, &zeros, "no body runs")?;

    let again_specs: Vec<(&'static str, &'static str, &'static str, bool)> =
        if with_prepared_sibling {
            vec![call_spec("a-again", "e1"), call_spec("b-again", "e1")]
        } else {
            vec![call_spec("b-again", "e1")]
        };
    let again_ids: Vec<&str> = again_specs.iter().map(|s| s.0).collect();
    let reissued = brain
        .run(batch_plan(
            &format!("{}-again", plan.batch_id),
            &again_specs,
        ))
        .await?;
    let expected_reissued: Vec<CallOutcome> =
        again_ids.iter().map(|id| result_of(id, "read")).collect();
    expect_equal(
        &reissued.outcomes,
        &expected_reissued,
        "a reissued call runs afresh",
    )?;
    ran_once(&h, &again_ids, &["shim", "hook", "body"], "exactly once")?;
    Ok(())
}

pub fn batch_cases() -> BTreeMap<&'static str, BatchCase> {
    let mut map: BTreeMap<&'static str, BatchCase> = BTreeMap::new();
    map.insert("BATCH-01", Arc::new(|a| Box::pin(crash_after_manifest(a))));
    map.insert("BATCH-02", Arc::new(|a| Box::pin(crash_after_prepares(a))));
    map.insert(
        "BATCH-03",
        Arc::new(|a| Box::pin(crash_inside_first_wave(a))),
    );
    map.insert(
        "BATCH-05",
        Arc::new(|a| Box::pin(executor_restart_keeps_prepared(a))),
    );
    map.insert(
        "BATCH-06",
        Arc::new(|a| Box::pin(lost_prepared_execution(a))),
    );
    map.insert(
        "BATCH-07",
        Arc::new(|a| Box::pin(waves_across_executors(a))),
    );
    map.insert("BATCH-08", Arc::new(|a| Box::pin(batch_abort(a))));
    map.insert(
        "BATCH-09",
        Arc::new(|a| Box::pin(blocked_beside_released(a))),
    );
    map.insert(
        "REPLAY-01",
        Arc::new(|a| Box::pin(replay_interrupted_preparation(a, false))),
    );
    map.insert(
        "REPLAY-02",
        Arc::new(|a| Box::pin(replay_interrupted_preparation(a, true))),
    );
    map
}

pub const BATCH_CASES: fn() -> BTreeMap<&'static str, BatchCase> = batch_cases;
