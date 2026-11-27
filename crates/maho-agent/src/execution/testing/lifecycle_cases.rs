//! Port of senpi `packages/agent/src/execution/testing/lifecycle-cases.ts`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::scope_lifecycle::{
    CloseScopeOptions, CloseScopeResult, LifecycleCommitResult, OpenScopeResult, ScopeBinding,
    ScopeError, ScopeRefusal, ScopeState,
};

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::batch_harness;
use super::nest_cases::assert_eq_val;

type LifecycleCase = CaseImplementation<BatchCaseAdapters>;

pub const IDLE_TTL_MS: i64 = 30_000;

pub fn binding_of(scope: &str, issuer: &str) -> ScopeBinding {
    ScopeBinding {
        issuer: issuer.to_string(),
        workspace: "workspace".to_string(),
        scope: scope.to_string(),
        generation: 1,
        allowed_roots: vec!["/work".to_string()],
    }
}

async fn commit_before_next_call(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let scopes = h.scopes(IDLE_TTL_MS, None).await?;
    scopes.open(binding_of("A", "brain")).await?;
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let gate_flag = Arc::new(AtomicBool::new(false));
    let gate_notify = Arc::new(Notify::new());

    let order_h = Arc::clone(&order);
    let gate_flag_h = Arc::clone(&gate_flag);
    let gate_notify_h = Arc::clone(&gate_notify);
    let scopes_e = Arc::clone(&scopes);
    let event = tokio::spawn(async move {
        scopes_e
            .lifecycle(
                "brain",
                "A",
                "agent_end",
                Box::new(move || {
                    Box::pin(async move {
                        order_h
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push("handler".to_string());
                        loop {
                            let notified = gate_notify_h.notified();
                            if gate_flag_h.load(Ordering::SeqCst) {
                                break;
                            }
                            notified.await;
                        }
                        Ok(json!({ "revoked": true }))
                    })
                }),
            )
            .await
    });

    // Yield once so lifecycle submits its turn onto the scope queue before call_scope is submitted.
    tokio::task::yield_now().await;

    let order_c = Arc::clone(&order);
    let scopes_c = Arc::clone(&scopes);
    let scopes_r = Arc::clone(&scopes);
    let call = tokio::spawn(async move {
        scopes_c
            .call_scope(
                "A",
                Box::new(move || {
                    Box::pin(async move {
                        let count = scopes_r
                            .read("A")
                            .await
                            .map_err(ScopeError::Other)?
                            .map(|r| r.events.len())
                            .unwrap_or(0);
                        order_c
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push(format!("call sees {count} committed event(s)"));
                        Ok(json!("ran"))
                    })
                }),
            )
            .await
    });

    gate_flag.store(true, Ordering::SeqCst);
    gate_notify.notify_waiters();

    let ev_res = event.await.map_err(|e| e.to_string())??;
    assert_eq_val(
        &ev_res,
        &LifecycleCommitResult::Committed { seq: 1 },
        "the event committed",
    )?;
    let call_res = call
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    expect_equal(&call_res, &json!("ran"), "the call ran")?;
    expect_equal(
        &*order.lock().unwrap_or_else(|p| p.into_inner()),
        &vec![
            "handler".to_string(),
            "call sees 1 committed event(s)".to_string(),
        ],
        "the next call follows the commit",
    )?;
    Ok(())
}

async fn issuer_only(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let scopes = h.scopes(IDLE_TTL_MS, None).await?;
    scopes.open(binding_of("A", "brain")).await?;
    let ran = Arc::new(AtomicBool::new(false));
    let ran_c = Arc::clone(&ran);
    let ev_res = scopes
        .lifecycle(
            "intruder",
            "A",
            "turn_start",
            Box::new(move || {
                Box::pin(async move {
                    ran_c.store(true, Ordering::SeqCst);
                    Ok(Value::Null)
                })
            }),
        )
        .await?;
    assert_eq_val(
        &ev_res,
        &LifecycleCommitResult::Refused(ScopeRefusal::NotIssuer),
        "a lifecycle event from another party",
    )?;
    expect_equal(&ran.load(Ordering::SeqCst), &false, "its handler never ran")?;
    assert_eq_val(
        &scopes
            .close("intruder", "A", CloseScopeOptions::default())
            .await?,
        &CloseScopeResult::Refused(ScopeRefusal::NotIssuer),
        "a close from another party",
    )?;
    expect_equal(
        &scopes.read("A").await?.map(|r| r.state),
        &Some(ScopeState::Open),
        "still open",
    )?;
    Ok(())
}

async fn closed_is_terminal(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let scopes = h.scopes(IDLE_TTL_MS, None).await?;
    scopes.open(binding_of("A", "brain")).await?;
    assert_eq_val(
        &scopes
            .close("brain", "A", CloseScopeOptions::default())
            .await?,
        &CloseScopeResult::Closed,
        "closed by its issuer",
    )?;
    assert_eq_val(
        &scopes
            .lifecycle(
                "brain",
                "A",
                "turn_start",
                Box::new(|| Box::pin(async { Ok(Value::Null) })),
            )
            .await?,
        &LifecycleCommitResult::Refused(ScopeRefusal::Closed),
        "no event after the close",
    )?;
    assert_eq_val(
        &scopes.open(binding_of("A", "brain")).await?,
        &OpenScopeResult::Refused(ScopeRefusal::ConflictingScope),
        "not reopened",
    )?;
    Ok(())
}

pub fn lifecycle_cases() -> BTreeMap<&'static str, LifecycleCase> {
    let mut map: BTreeMap<&'static str, LifecycleCase> = BTreeMap::new();
    map.insert(
        "LIFECYCLE-01",
        Arc::new(|a| Box::pin(commit_before_next_call(a))),
    );
    map.insert("LIFECYCLE-02", Arc::new(|a| Box::pin(issuer_only(a))));
    map.insert(
        "LIFECYCLE-03",
        Arc::new(|a| Box::pin(closed_is_terminal(a))),
    );
    map
}

pub const LIFECYCLE_CASES: fn() -> BTreeMap<&'static str, LifecycleCase> = lifecycle_cases;
