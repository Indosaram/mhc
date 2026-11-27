//! Port of senpi `packages/agent/src/execution/testing/scope-cases.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::scope_lifecycle::{
    CloseScopeOptions, CloseScopeResult, ScopeRefusal, ScopeState, TerminateResourceFn,
};

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::{BrainKill, BrainKillAt, batch_harness, expect_rejects_msg};
use super::lifecycle_cases::{IDLE_TTL_MS, binding_of};
use super::nest_cases::assert_eq_val;

type ScopeCase = CaseImplementation<BatchCaseAdapters>;

async fn ttl_spares_live(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let scopes = h.scopes(IDLE_TTL_MS, None).await?;
    scopes.open(binding_of("live", "brain")).await?;
    scopes.open(binding_of("idle", "brain")).await?;
    scopes
        .hold("live", "bash_1", "terminal")
        .await
        .map_err(|e| e.to_string())?;
    h.advance(IDLE_TTL_MS + 1);
    expect_equal(
        &scopes.sweep_idle().await?,
        &vec!["idle".to_string()],
        "only the scope without live resources closes",
    )?;
    expect_equal(
        &scopes.read("live").await?.map(|r| r.state),
        &Some(ScopeState::Open),
        "the live scope stays open",
    )?;
    scopes.release("live", "bash_1").await?;
    expect_equal(
        &scopes.sweep_idle().await?,
        &Vec::<String>::new(),
        "a release is activity: the TTL starts again",
    )?;
    h.advance(IDLE_TTL_MS + 1);
    expect_equal(
        &scopes.sweep_idle().await?,
        &vec!["live".to_string()],
        "then it closes",
    )?;
    Ok(())
}

async fn forced_close_outcomes_first(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let scopes = h.scopes(IDLE_TTL_MS, None).await?;
    scopes.open(binding_of("A", "brain")).await?;
    scopes
        .hold("A", "r1", "execution")
        .await
        .map_err(|e| e.to_string())?;
    scopes
        .hold("A", "r2", "terminal")
        .await
        .map_err(|e| e.to_string())?;
    assert_eq_val(
        &scopes
            .close("brain", "A", CloseScopeOptions::default())
            .await?,
        &CloseScopeResult::Refused(ScopeRefusal::LiveResources),
        "not without force",
    )?;
    let terminated = Arc::new(Mutex::new(Vec::<String>::new()));
    let terminated_c = Arc::clone(&terminated);
    let terminate: TerminateResourceFn = Arc::new(move |resource| {
        let term_inner = Arc::clone(&terminated_c);
        Box::pin(async move {
            term_inner
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(resource.clone());
            Ok(json!({ "reason": "cancelled", "resource": resource }))
        })
    });
    let dying = h
        .scopes(
            IDLE_TTL_MS,
            Some(BrainKill {
                label: "outcome:r1".to_string(),
                at: BrainKillAt::After,
            }),
        )
        .await?;
    expect_rejects_msg(
        dying.close(
            "brain",
            "A",
            CloseScopeOptions {
                force: true,
                terminate: Some(Arc::clone(&terminate)),
            },
        ),
        "brain killed around",
        "dies after r1",
    )
    .await?;
    let midway = h.scopes(IDLE_TTL_MS, None).await?.read("A").await?;
    let midway_keys: Option<Vec<String>> = midway
        .as_ref()
        .map(|r| r.outcomes.keys().cloned().collect());
    expect_equal(
        &json!([midway.as_ref().map(|r| r.state), midway_keys]),
        &json!(["open", ["r1"]]),
        "r1's outcome, still open",
    )?;
    let restarted = h.scopes(IDLE_TTL_MS, None).await?;
    assert_eq_val(
        &restarted
            .close(
                "brain",
                "A",
                CloseScopeOptions {
                    force: true,
                    terminate: Some(terminate),
                },
            )
            .await?,
        &CloseScopeResult::Closed,
        "closed",
    )?;
    expect_equal(
        &*terminated.lock().unwrap_or_else(|p| p.into_inner()),
        &vec!["r1".to_string(), "r2".to_string()],
        "each resource terminated once",
    )?;
    let closed = restarted.read("A").await?;
    let mut closed_keys: Vec<String> = closed
        .as_ref()
        .map(|r| r.outcomes.keys().cloned().collect())
        .unwrap_or_default();
    closed_keys.sort();
    expect_equal(
        &json!([closed.as_ref().map(|r| r.state), closed_keys]),
        &json!(["closed", ["r1", "r2"]]),
        "outcomes first",
    )?;
    Ok(())
}

pub fn scope_cases() -> BTreeMap<&'static str, ScopeCase> {
    let mut map: BTreeMap<&'static str, ScopeCase> = BTreeMap::new();
    map.insert("SCOPE-01", Arc::new(|a| Box::pin(ttl_spares_live(a))));
    map.insert(
        "SCOPE-02",
        Arc::new(|a| Box::pin(forced_close_outcomes_first(a))),
    );
    map
}

pub const SCOPE_CASES: fn() -> BTreeMap<&'static str, ScopeCase> = scope_cases;
