//! Port of senpi `packages/agent/src/execution/testing/nest-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::coordinator_types::ExecutionRef;
use crate::execution::nested::{
    NestedAdmissionControl, OpenNestedResult, RegisterActivityResult, nested_child_id,
};
use crate::execution::nested_types::{
    ActivityRefusal, NESTED_LIFETIME_MS, NestedActivity, NestedAdmission, NestedCapabilities,
    NestedParent, NestedRefusal, NestedRequest,
};

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::{BatchHarness, batch_harness};

type NestCase = CaseImplementation<BatchCaseAdapters>;

const NEST_ADMITTED_AT: i64 = 101;
const LEASE_MS: i64 = 60_000;

pub fn nest_parent() -> NestedParent {
    NestedParent {
        execution_id: "p".to_string(),
        generation: 1,
        executor: "e1".to_string(),
        scope: "scope".to_string(),
        admitted_at: NEST_ADMITTED_AT,
        lease_until: NEST_ADMITTED_AT + LEASE_MS,
        budget: 10,
        capabilities: NestedCapabilities {
            preparation: vec!["model".to_string()],
            body: vec!["e2".to_string()],
        },
    }
}

pub fn nest_request() -> NestedRequest {
    NestedRequest {
        sender: "e1".to_string(),
        parent: "p".to_string(),
        parent_generation: 1,
        scope: "scope".to_string(),
        child_request_id: "c1".to_string(),
        target: "model".to_string(),
        cost: 1,
        activity_id: None,
    }
}

pub fn admitted(child_request_id: &str, target: &str, replayed: bool) -> NestedAdmission {
    NestedAdmission::Admitted {
        child: ExecutionRef {
            issuer: "brain".to_string(),
            execution_id: nested_child_id("p", child_request_id),
            generation: 1,
        },
        target: target.to_string(),
        replayed,
    }
}

pub fn refused_with(reason: NestedRefusal) -> NestedAdmission {
    NestedAdmission::Refused { reason }
}

pub fn assert_eq_val<T: PartialEq + std::fmt::Debug>(
    actual: &T,
    expected: &T,
    what: &str,
) -> Result<(), String> {
    if actual != expected {
        return Err(format!("{what}: expected {expected:?}, got {actual:?}"));
    }
    Ok(())
}

pub async fn opened_parent(
    adapters: BatchCaseAdapters,
) -> Result<(BatchHarness, Arc<dyn NestedAdmissionControl>), String> {
    let h = batch_harness(adapters).await?;
    let nested = h.nested(None, None).await?;
    nested.open(nest_parent()).await?;
    Ok((h, nested))
}

pub async fn debited(nested: &dyn NestedAdmissionControl) -> Result<Option<u64>, String> {
    Ok(nested.read("p").await?.map(|r| r.debited))
}

async fn wrong_hand(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (_h, nested) = opened_parent(adapters).await?;
    let mut other = nest_request();
    other.sender = "e2".to_string();
    assert_eq_val(
        &nested.admit(other).await?,
        &refused_with(NestedRefusal::NotParentExecutor),
        "another hand",
    )?;
    let mut forged = nest_request();
    forged.parent = "forged".to_string();
    assert_eq_val(
        &nested.admit(forged).await?,
        &refused_with(NestedRefusal::UnknownParent),
        "forged parent",
    )?;
    expect_equal(
        &debited(nested.as_ref()).await?,
        &Some(0u64),
        "nothing debited",
    )?;
    assert_eq_val(
        &nested.admit(nest_request()).await?,
        &admitted("c1", "model", false),
        "the parent's executor is admitted",
    )?;
    Ok(())
}

async fn replay(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (h, nested) = opened_parent(adapters).await?;
    assert_eq_val(
        &nested.admit(nest_request()).await?,
        &admitted("c1", "model", false),
        "first admission",
    )?;
    assert_eq_val(
        &nested.admit(nest_request()).await?,
        &admitted("c1", "model", true),
        "a replay, same child",
    )?;
    let restarted = h.nested(None, None).await?;
    assert_eq_val(
        &restarted.admit(nest_request()).await?,
        &admitted("c1", "model", true),
        "a replay after a restart",
    )?;
    expect_equal(
        &debited(restarted.as_ref()).await?,
        &Some(1u64),
        "debited once",
    )?;
    let mut changed_target = nest_request();
    changed_target.target = "e2".to_string();
    assert_eq_val(
        &restarted.admit(changed_target).await?,
        &refused_with(NestedRefusal::ConflictingRequest),
        "the same child request id for another call",
    )?;
    let mut px = nest_parent();
    px.execution_id = "p/x".to_string();
    assert_eq_val(
        &restarted.open(px).await?,
        &OpenNestedResult::Opened,
        "a parent named p/x",
    )?;
    let mut r_px = nest_request();
    r_px.parent = "p/x".to_string();
    let nested_name = restarted.admit(r_px).await?;
    let mut r_slashed = nest_request();
    r_slashed.child_request_id = "x/c1".to_string();
    let slashed_request = restarted.admit(r_slashed).await?;
    let (
        NestedAdmission::Admitted { child: c_a, .. },
        NestedAdmission::Admitted { child: c_b, .. },
    ) = (nested_name, slashed_request)
    else {
        return Err("NEST-02: not admitted".to_string());
    };
    if c_a.execution_id == c_b.execution_id {
        return Err(format!("NEST-02: two children share {}", c_a.execution_id));
    }
    Ok(())
}

async fn sibling_budget(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (_h, nested) = opened_parent(adapters).await?;
    let mut r1 = nest_request();
    r1.cost = 6;
    assert_eq_val(
        &matches!(nested.admit(r1).await?, NestedAdmission::Admitted { .. }),
        &true,
        "the first sibling fits",
    )?;
    let mut r2 = nest_request();
    r2.child_request_id = "c2".to_string();
    r2.cost = 5;
    assert_eq_val(
        &nested.admit(r2).await?,
        &refused_with(NestedRefusal::BudgetExhausted),
        "6 + 5 exceeds 10",
    )?;
    expect_equal(
        &debited(nested.as_ref()).await?,
        &Some(6u64),
        "the refused sibling debited nothing",
    )?;
    let mut r3 = nest_request();
    r3.child_request_id = "c3".to_string();
    r3.cost = 4;
    assert_eq_val(
        &matches!(nested.admit(r3).await?, NestedAdmission::Admitted { .. }),
        &true,
        "6 + 4 fits",
    )?;
    for cost in [-100i64, -1] {
        let mut bad = nest_request();
        bad.child_request_id = format!("bad-{cost}");
        bad.cost = cost;
        assert_eq_val(
            &nested.admit(bad).await?,
            &refused_with(NestedRefusal::InvalidCost),
            &format!("cost {cost}"),
        )?;
    }
    expect_equal(
        &debited(nested.as_ref()).await?,
        &Some(10u64),
        "an invalid cost never moves the reservation",
    )?;
    expect_equal(
        &nested.cancel("p").await?.len(),
        &2usize,
        "the record still reads: the cancel names both children",
    )?;
    Ok(())
}

async fn generation_and_scope(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (_h, nested) = opened_parent(adapters).await?;
    let mut stale = nest_request();
    stale.parent_generation = 2;
    assert_eq_val(
        &nested.admit(stale).await?,
        &refused_with(NestedRefusal::StaleGeneration),
        "generation",
    )?;
    let mut wrong_scope = nest_request();
    wrong_scope.scope = "other".to_string();
    assert_eq_val(
        &nested.admit(wrong_scope).await?,
        &refused_with(NestedRefusal::ScopeMismatch),
        "scope",
    )?;
    assert_eq_val(
        &nested.open(nest_parent()).await?,
        &OpenNestedResult::Opened,
        "opening the same parent again",
    )?;
    let mut conflict = nest_parent();
    conflict.generation = 2;
    assert_eq_val(
        &nested.open(conflict).await?,
        &OpenNestedResult::RefusedConflictingParent,
        "opening it again with another generation",
    )?;
    assert_eq_val(
        &nested.open(nest_parent()).await?,
        &OpenNestedResult::Opened,
        "the same parent with its fields in another order",
    )?;
    Ok(())
}

async fn preparation_phase(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (_h, nested) = opened_parent(adapters).await?;
    assert_eq_val(
        &nested.admit(nest_request()).await?,
        &admitted("c1", "model", false),
        "a preflight cross-plane call",
    )?;
    let mut r2 = nest_request();
    r2.child_request_id = "c2".to_string();
    r2.target = "e2".to_string();
    assert_eq_val(
        &nested.admit(r2).await?,
        &refused_with(NestedRefusal::NotInNestedSet),
        "a body-phase target before release",
    )?;
    Ok(())
}

async fn release_widens(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (_h, nested) = opened_parent(adapters).await?;
    nested.release("p").await?;
    let mut r2 = nest_request();
    r2.child_request_id = "c2".to_string();
    r2.target = "e2".to_string();
    assert_eq_val(
        &nested.admit(r2).await?,
        &admitted("c2", "e2", false),
        "a body-phase target after release",
    )?;
    assert_eq_val(
        &matches!(
            nested.admit(nest_request()).await?,
            NestedAdmission::Admitted { .. }
        ),
        &true,
        "the preparation targets stay",
    )?;
    Ok(())
}

async fn detached_activity(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (h, nested) = opened_parent(adapters).await?;
    let parent = nest_parent();
    let expires_at = parent.lease_until;
    let base_activity = NestedActivity {
        activity_id: "a1".to_string(),
        parent: "p".to_string(),
        executor: "e1".to_string(),
        scope: "scope".to_string(),
        targets: vec!["model".to_string()],
        budget: 3,
        expires_at,
    };
    let mut early = base_activity.clone();
    early.activity_id = "early".to_string();
    early.targets = vec!["e2".to_string()];
    assert_eq_val(
        &nested.register_activity(early).await?,
        &RegisterActivityResult::Refused(ActivityRefusal::ExceedsParent),
        "a body-phase target before release",
    )?;
    nested.release("p").await?;

    let mut bad_target = base_activity.clone();
    bad_target.targets = vec!["model".to_string(), "elsewhere".to_string()];
    let mut bad_expiry = base_activity.clone();
    bad_expiry.expires_at = expires_at + 1;
    let mut bad_budget = base_activity.clone();
    bad_budget.budget = 11;
    let mut bad_hand = base_activity.clone();
    bad_hand.executor = "e2".to_string();

    for (over, what) in [
        (bad_target, "a target the parent does not hold"),
        (bad_expiry, "an expiry past the parent's"),
        (bad_budget, "a budget over the parent's remaining"),
        (bad_hand, "another hand"),
    ] {
        assert_eq_val(
            &nested.register_activity(over).await?,
            &RegisterActivityResult::Refused(ActivityRefusal::ExceedsParent),
            what,
        )?;
    }

    assert_eq_val(
        &nested.register_activity(base_activity.clone()).await?,
        &RegisterActivityResult::Registered,
        "registered",
    )?;
    nested.settle("p").await?;
    assert_eq_val(
        &nested.admit(nest_request()).await?,
        &refused_with(NestedRefusal::ParentTerminal),
        "the terminal invocation",
    )?;
    let mut act_req = nest_request();
    act_req.activity_id = Some("a1".to_string());
    assert_eq_val(
        &nested.admit(act_req).await?,
        &admitted("c1", "model", false),
        "the live activity",
    )?;
    let mut out_subset = nest_request();
    out_subset.activity_id = Some("a1".to_string());
    out_subset.child_request_id = "c2".to_string();
    out_subset.target = "e2".to_string();
    assert_eq_val(
        &nested.admit(out_subset).await?,
        &refused_with(NestedRefusal::NotInNestedSet),
        "outside the activity's subset",
    )?;
    let mut unknown_act = nest_request();
    unknown_act.activity_id = Some("a9".to_string());
    assert_eq_val(
        &nested.admit(unknown_act).await?,
        &refused_with(NestedRefusal::UnknownActivity),
        "no such cell",
    )?;
    nested.end_activity("p", "a1").await?;
    let mut after_end = nest_request();
    after_end.activity_id = Some("a1".to_string());
    after_end.child_request_id = "c3".to_string();
    assert_eq_val(
        &nested.admit(after_end).await?,
        &refused_with(NestedRefusal::ActivityEnded),
        "after the cell ended",
    )?;
    let mut late = base_activity.clone();
    late.activity_id = "late".to_string();
    assert_eq_val(
        &nested.register_activity(late).await?,
        &RegisterActivityResult::Refused(ActivityRefusal::ParentTerminal),
        "no activity revives an invocation that ended",
    )?;

    let fresh = h.nested(None, None).await?;
    let mut q = parent;
    q.execution_id = "q".to_string();
    fresh.open(q).await?;
    h.advance(LEASE_MS + 1);
    let mut exp_act = base_activity;
    exp_act.parent = "q".to_string();
    exp_act.activity_id = "expired".to_string();
    assert_eq_val(
        &fresh.register_activity(exp_act).await?,
        &RegisterActivityResult::Refused(ActivityRefusal::ParentTerminal),
        "nor one whose parent's set expired",
    )?;
    Ok(())
}

async fn activity_budget(adapters: BatchCaseAdapters) -> ConformanceResult {
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
    for id in ["c1", "c2"] {
        let mut req = nest_request();
        req.activity_id = Some("a1".to_string());
        req.child_request_id = id.to_string();
        assert_eq_val(
            &matches!(nested.admit(req).await?, NestedAdmission::Admitted { .. }),
            &true,
            id,
        )?;
    }
    let mut c3 = nest_request();
    c3.activity_id = Some("a1".to_string());
    c3.child_request_id = "c3".to_string();
    assert_eq_val(
        &nested.admit(c3).await?,
        &refused_with(NestedRefusal::BudgetExhausted),
        "the activity's budget",
    )?;
    let mut c4 = nest_request();
    c4.child_request_id = "c4".to_string();
    c4.cost = 8;
    assert_eq_val(
        &matches!(nested.admit(c4).await?, NestedAdmission::Admitted { .. }),
        &true,
        "2 + 8 fits",
    )?;
    let mut c5 = nest_request();
    c5.child_request_id = "c5".to_string();
    assert_eq_val(
        &nested.admit(c5).await?,
        &refused_with(NestedRefusal::BudgetExhausted),
        "the parent's reservation counts the activity's children",
    )?;
    Ok(())
}

async fn lease_follows_parent(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (h, nested) = opened_parent(adapters).await?;
    let parent = nest_parent();
    h.advance(LEASE_MS + 1);
    assert_eq_val(
        &nested.admit(nest_request()).await?,
        &refused_with(NestedRefusal::Expired),
        "past the parent's lease",
    )?;
    expect_equal(
        &nested.renew("p", parent.lease_until + LEASE_MS).await?,
        &(parent.lease_until + LEASE_MS),
        "renewed",
    )?;
    assert_eq_val(
        &matches!(
            nested.admit(nest_request()).await?,
            NestedAdmission::Admitted { .. }
        ),
        &true,
        "admitted after the renewal",
    )?;
    expect_equal(
        &nested.renew("p", NEST_ADMITTED_AT).await?,
        &(parent.lease_until + LEASE_MS),
        "never moves back",
    )?;
    expect_equal(
        &nested
            .renew("p", NEST_ADMITTED_AT + 2 * NESTED_LIFETIME_MS)
            .await?,
        &(NEST_ADMITTED_AT + NESTED_LIFETIME_MS),
        "capped at the parent's lifetime",
    )?;
    Ok(())
}

pub fn nest_cases() -> BTreeMap<&'static str, NestCase> {
    let mut map: BTreeMap<&'static str, NestCase> = BTreeMap::new();
    map.insert("NEST-01", Arc::new(|a| Box::pin(wrong_hand(a))));
    map.insert("NEST-02", Arc::new(|a| Box::pin(replay(a))));
    map.insert("NEST-03", Arc::new(|a| Box::pin(sibling_budget(a))));
    map.insert("NEST-04", Arc::new(|a| Box::pin(generation_and_scope(a))));
    map.insert("NEST-05", Arc::new(|a| Box::pin(preparation_phase(a))));
    map.insert("NEST-06", Arc::new(|a| Box::pin(release_widens(a))));
    map.insert("NEST-07", Arc::new(|a| Box::pin(detached_activity(a))));
    map.insert("NEST-08", Arc::new(|a| Box::pin(activity_budget(a))));
    map.insert("NEST-09", Arc::new(|a| Box::pin(lease_follows_parent(a))));
    map
}

pub const NEST_CASES: fn() -> BTreeMap<&'static str, NestCase> = nest_cases;
