//! Port of senpi `packages/agent/src/execution/testing/multi-seq-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::coordinator_sequential::create_sequential_coordinator;

use super::batch_executor::{BatchCaseAdapters, PausePoint};
use super::batch_harness::{batch_harness, batch_plan, call_spec, result_of};

const STARVED_MS: u64 = 20_000;

async fn long_sequential_does_not_starve(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let held = h.hold("a1", PausePoint::Body);
    let session_a = create_sequential_coordinator(h.brain(None, Some("brain-a"), None).await?);
    let plan_a = batch_plan(
        "multi-fair-02-a",
        &[
            call_spec("a1", "e1"),
            call_spec("a2", "e1"),
            call_spec("a3", "e1"),
        ],
    );
    let run_a = tokio::spawn(async move { session_a.run(plan_a).await });
    held.reached().await;

    let session_b = h.brain(None, Some("brain-b"), None).await?;
    let plan_b = batch_plan(
        "multi-fair-02-b",
        &[call_spec("b1", "e1"), call_spec("b2", "e1")],
    );
    let result_b = tokio::time::timeout(Duration::from_millis(STARVED_MS), session_b.run(plan_b))
        .await
        .map_err(|_| "MULTI-FAIR-02: B did not finish while A's call was held".to_string())??;
    expect_equal(
        &result_b.outcomes,
        &vec![result_of("b1", "read"), result_of("b2", "read")],
        "B finishes while A's call is held",
    )?;
    expect_equal(
        &h.count("shim:a2"),
        &0usize,
        "A's next call waits for its own held call, not for B",
    )?;
    held.open();
    let result_a = run_a.await.map_err(|e| e.to_string())??;
    expect_equal(
        &result_a.outcomes,
        &vec![
            result_of("a1", "read"),
            result_of("a2", "read"),
            result_of("a3", "read"),
        ],
        "A finishes in its order",
    )?;
    let trace = h.trace();
    let order = |step: &str| trace.iter().position(|s| s == step);
    let (Some(b2), Some(a1), Some(sa2), Some(da2), Some(sa3)) = (
        order("done:b2"),
        order("done:a1"),
        order("shim:a2"),
        order("done:a2"),
        order("shim:a3"),
    ) else {
        return Err(format!(
            "MULTI-FAIR-02: unexpected order {}",
            trace.join(",")
        ));
    };
    if !(b2 < a1 && a1 < sa2 && da2 < sa3) {
        return Err(format!(
            "MULTI-FAIR-02: unexpected order {}",
            trace.join(",")
        ));
    }
    Ok(())
}

pub fn multi_seq_cases() -> BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> = BTreeMap::new();
    map.insert(
        "MULTI-FAIR-02",
        Arc::new(|a| Box::pin(long_sequential_does_not_starve(a))),
    );
    map
}

pub const MULTI_SEQ_CASES: fn() -> BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> =
    multi_seq_cases;
