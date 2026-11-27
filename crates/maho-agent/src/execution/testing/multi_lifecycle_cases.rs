//! Port of senpi `packages/agent/src/execution/testing/multi-lifecycle-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::scope_lifecycle::{LifecycleCommitResult, ScopeRefusal};

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::batch_harness;
use super::lifecycle_cases::{IDLE_TTL_MS, binding_of};
use super::nest_cases::assert_eq_val;

async fn lifecycle_stays_in_session(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let scopes = h.scopes(IDLE_TTL_MS, None).await?;
    scopes.open(binding_of("A", "brain-a")).await?;
    scopes.open(binding_of("B", "brain-b")).await?;
    scopes
        .hold("B", "computer", "lease")
        .await
        .map_err(|e| e.to_string())?;
    let held_flag = Arc::new(AtomicBool::new(false));
    let held_notify = Arc::new(Notify::new());
    let flag_c = Arc::clone(&held_flag);
    let notify_c = Arc::clone(&held_notify);
    let scopes_a = Arc::clone(&scopes);
    let end_a = tokio::spawn(async move {
        scopes_a
            .lifecycle(
                "brain-a",
                "A",
                "agent_end",
                Box::new(move || {
                    Box::pin(async move {
                        loop {
                            let notified = notify_c.notified();
                            if flag_c.load(Ordering::SeqCst) {
                                break;
                            }
                            notified.await;
                        }
                        Ok(json!({ "revoked": "computer" }))
                    })
                }),
            )
            .await
    });
    let b_ran = scopes
        .call_scope("B", Box::new(|| Box::pin(async { Ok(json!("B ran")) })))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &b_ran,
        &json!("B ran"),
        "B's call does not wait on A's handler",
    )?;
    held_flag.store(true, Ordering::SeqCst);
    held_notify.notify_waiters();
    end_a.await.map_err(|e| e.to_string())??;
    scopes
        .lifecycle(
            "brain-a",
            "A",
            "turn_start",
            Box::new(|| Box::pin(async { Ok(json!({ "reset": "comment-checker" })) })),
        )
        .await?;
    let a_events: Vec<(String, Value)> = scopes
        .read("A")
        .await?
        .map(|r| {
            r.events
                .into_iter()
                .map(|ev| (ev.kind, ev.effect))
                .collect()
        })
        .unwrap_or_default();
    expect_equal(
        &a_events,
        &vec![
            ("agent_end".to_string(), json!({ "revoked": "computer" })),
            (
                "turn_start".to_string(),
                json!({ "reset": "comment-checker" }),
            ),
        ],
        "A's effects in A",
    )?;
    let b = scopes.read("B").await?;
    let b_resources: BTreeMap<String, String> =
        b.as_ref().map(|r| r.resources.clone()).unwrap_or_default();
    let b_events = b.map(|r| r.events).unwrap_or_default();
    expect_equal(
        &json!([b_events, b_resources]),
        &json!([[], { "computer": "lease" }]),
        "B is untouched",
    )?;
    assert_eq_val(
        &scopes
            .lifecycle(
                "brain-a",
                "B",
                "agent_end",
                Box::new(|| Box::pin(async { Ok(Value::Null) })),
            )
            .await?,
        &LifecycleCommitResult::Refused(ScopeRefusal::NotIssuer),
        "A's brain cannot end B",
    )?;
    Ok(())
}

pub fn multi_lifecycle_cases() -> BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<BatchCaseAdapters>> = BTreeMap::new();
    map.insert(
        "MULTI-LIFE-01",
        Arc::new(|a| Box::pin(lifecycle_stays_in_session(a))),
    );
    map
}

pub const MULTI_LIFECYCLE_CASES: fn() -> BTreeMap<
    &'static str,
    CaseImplementation<BatchCaseAdapters>,
> = multi_lifecycle_cases;
