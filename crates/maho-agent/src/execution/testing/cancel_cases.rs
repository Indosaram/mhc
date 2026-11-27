//! Port of senpi `packages/agent/src/execution/testing/cancel-cases.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::coordinator_types::ExecutionRef;
use crate::execution::nested::{RegisterActivityResult, nested_child_id};
use crate::execution::nested_types::{
    ActivityRefusal, CancelDeliveryState, NestedActivity, NestedCancelTarget, NestedRefusal,
};

use super::batch_executor::BatchCaseAdapters;
use super::nest_cases::{
    admitted, assert_eq_val, nest_parent, nest_request, opened_parent, refused_with,
};

type CancelCase = CaseImplementation<BatchCaseAdapters>;

pub fn cancel_target_of(child_request_id: &str, executor: &str) -> NestedCancelTarget {
    NestedCancelTarget {
        exec_ref: ExecutionRef {
            issuer: "brain".to_string(),
            execution_id: nested_child_id("p", child_request_id),
            generation: 1,
        },
        executor: executor.to_string(),
    }
}

async fn cancel_first(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (_h, nested) = opened_parent(adapters).await?;
    let parent = nest_parent();
    nested.release("p").await?;
    nested
        .register_activity(NestedActivity {
            activity_id: "a1".to_string(),
            parent: "p".to_string(),
            executor: "e1".to_string(),
            scope: "scope".to_string(),
            targets: vec!["model".to_string()],
            budget: 2,
            expires_at: parent.lease_until,
        })
        .await?;
    assert_eq_val(
        &nested.cancel("p").await?,
        &Vec::new(),
        "no child to cascade to",
    )?;
    assert_eq_val(
        &nested.admit(nest_request()).await?,
        &refused_with(NestedRefusal::ParentTerminal),
        "a child racing the cancel",
    )?;
    let mut act_req = nest_request();
    act_req.activity_id = Some("a1".to_string());
    assert_eq_val(
        &nested.admit(act_req).await?,
        &refused_with(NestedRefusal::ActivityEnded),
        "the cancel binding ended the activity",
    )?;
    assert_eq_val(
        &nested
            .register_activity(NestedActivity {
                activity_id: "a2".to_string(),
                parent: "p".to_string(),
                executor: "e1".to_string(),
                scope: "scope".to_string(),
                targets: vec!["model".to_string()],
                budget: 1,
                expires_at: parent.lease_until,
            })
            .await?,
        &RegisterActivityResult::Refused(ActivityRefusal::ParentTerminal),
        "no new activity after the cancel",
    )?;
    Ok(())
}

async fn admitted_first(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (h, nested) = opened_parent(adapters).await?;
    nested.release("p").await?;
    let mut r1 = nest_request();
    r1.target = "e2".to_string();
    assert_eq_val(
        &nested.admit(r1.clone()).await?,
        &admitted("c1", "e2", false),
        "admitted before the cancel",
    )?;
    assert_eq_val(
        &nested.cancel("p").await?,
        &vec![cancel_target_of("c1", "e2")],
        "the cancel names the child",
    )?;
    let mut expected_cancels = BTreeMap::new();
    expected_cancels.insert(nested_child_id("p", "c1"), CancelDeliveryState::Queued);
    assert_eq_val(
        &nested.read("p").await?.map(|r| r.cancels),
        &Some(expected_cancels),
        "the cascade is durable before delivery",
    )?;
    let refused = Arc::new(Mutex::new(Vec::new()));
    let refused_c = Arc::clone(&refused);
    let _ = nested
        .deliver_cancels(
            "p",
            Arc::new(move |target| {
                let refused_inner = Arc::clone(&refused_c);
                Box::pin(async move {
                    refused_inner
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(target);
                    Err("the child's executor is unreachable".to_string())
                })
            }),
        )
        .await;

    let restarted = h.nested(None, None).await?;
    let sent = Arc::new(Mutex::new(Vec::new()));
    let sent_c = Arc::clone(&sent);
    let delivered = restarted
        .deliver_cancels(
            "p",
            Arc::new(move |target| {
                let sent_inner = Arc::clone(&sent_c);
                Box::pin(async move {
                    sent_inner
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(target);
                    Ok(())
                })
            }),
        )
        .await?;
    expect_equal(&delivered, &1usize, "a restarted brain re-sends it")?;
    assert_eq_val(
        &*refused.lock().unwrap_or_else(|p| p.into_inner()),
        &vec![cancel_target_of("c1", "e2")],
        "refused send",
    )?;
    assert_eq_val(
        &*sent.lock().unwrap_or_else(|p| p.into_inner()),
        &vec![cancel_target_of("c1", "e2")],
        "restarted send",
    )?;
    expect_equal(
        &restarted
            .deliver_cancels("p", Arc::new(|_| Box::pin(async { Ok(()) })))
            .await?,
        &0usize,
        "delivered once taken",
    )?;
    assert_eq_val(
        &restarted.admit(r1).await?,
        &admitted("c1", "e2", true),
        "the admitted child still exists",
    )?;
    Ok(())
}

pub fn cancel_cases() -> BTreeMap<&'static str, CancelCase> {
    let mut map: BTreeMap<&'static str, CancelCase> = BTreeMap::new();
    map.insert("CANCEL-01", Arc::new(|a| Box::pin(cancel_first(a))));
    map.insert("CANCEL-02", Arc::new(|a| Box::pin(admitted_first(a))));
    map
}

pub const CANCEL_CASES: fn() -> BTreeMap<&'static str, CancelCase> = cancel_cases;
