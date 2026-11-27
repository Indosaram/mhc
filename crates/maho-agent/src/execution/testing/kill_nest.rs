//! Port of senpi `packages/agent/src/execution/testing/kill-nest.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::nested::{NestedAdmissionControl, nested_child_id};
use crate::execution::nested_types::{CancelDeliveryState, NestedCancelTarget};

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::{
    BatchHarness, BrainKill, BrainKillAt, batch_harness, expect_rejects_msg,
};
use super::cancel_cases::cancel_target_of;
use super::nest_cases::{admitted, assert_eq_val, debited, nest_parent, nest_request};

type KillNestCase = CaseImplementation<BatchCaseAdapters>;

async fn killed_around(
    adapters: BatchCaseAdapters,
    label: String,
    at: BrainKillAt,
) -> Result<(BatchHarness, Arc<dyn NestedAdmissionControl>), String> {
    let h = batch_harness(adapters).await?;
    h.nested(None, None).await?.open(nest_parent()).await?;
    h.nested(None, None).await?.release("p").await?;
    let dying = h.nested(Some(BrainKill { label, at }), None).await?;
    Ok((h, dying))
}

async fn before_admission(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (h, dying) = killed_around(
        adapters,
        format!("admit:{}", nested_child_id("p", "c1")),
        BrainKillAt::Before,
    )
    .await?;
    expect_rejects_msg(
        dying.admit(nest_request()),
        "brain killed around",
        "the dying admission",
    )
    .await?;
    let restarted = h.nested(None, None).await?;
    expect_equal(
        &debited(restarted.as_ref()).await?,
        &Some(0u64),
        "nothing durable",
    )?;
    assert_eq_val(
        &restarted.admit(nest_request()).await?,
        &admitted("c1", "model", false),
        "admitted on the retry",
    )?;
    expect_equal(
        &debited(restarted.as_ref()).await?,
        &Some(1u64),
        "debited once",
    )?;
    Ok(())
}

async fn after_admission(adapters: BatchCaseAdapters) -> ConformanceResult {
    let (h, dying) = killed_around(
        adapters,
        format!("admit:{}", nested_child_id("p", "c1")),
        BrainKillAt::After,
    )
    .await?;
    expect_rejects_msg(
        dying.admit(nest_request()),
        "brain killed around",
        "the dying admission",
    )
    .await?;
    let restarted = h.nested(None, None).await?;
    assert_eq_val(
        &restarted.admit(nest_request()).await?,
        &admitted("c1", "model", true),
        "the same child",
    )?;
    expect_equal(
        &debited(restarted.as_ref()).await?,
        &Some(1u64),
        "never debited twice",
    )?;
    Ok(())
}

async fn around_cascade(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let nested = h.nested(None, None).await?;
    nested.open(nest_parent()).await?;
    nested.release("p").await?;
    let mut r1 = nest_request();
    r1.target = "e2".to_string();
    nested.admit(r1).await?;

    expect_rejects_msg(
        h.nested(
            Some(BrainKill {
                label: "cancel".to_string(),
                at: BrainKillAt::After,
            }),
            None,
        )
        .await?
        .cancel("p"),
        "brain killed around",
        "dies after the cancel",
    )
    .await?;
    let mut expected_cancels = BTreeMap::new();
    expected_cancels.insert(nested_child_id("p", "c1"), CancelDeliveryState::Queued);
    assert_eq_val(
        &h.nested(None, None)
            .await?
            .read("p")
            .await?
            .map(|r| r.cancels),
        &Some(expected_cancels),
        "the cascade is durable",
    )?;

    let sent = Arc::new(Mutex::new(Vec::<NestedCancelTarget>::new()));
    let sent_c = Arc::clone(&sent);
    let send_fn: crate::execution::nested::DeliverCancelFn = Arc::new(move |target| {
        let sent_inner = Arc::clone(&sent_c);
        Box::pin(async move {
            sent_inner
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(target);
            Ok(())
        })
    });
    expect_rejects_msg(
        h.nested(
            Some(BrainKill {
                label: format!("cancel_delivered:{}", nested_child_id("p", "c1")),
                at: BrainKillAt::Before,
            }),
            None,
        )
        .await?
        .deliver_cancels("p", Arc::clone(&send_fn)),
        "brain killed around",
        "dies after sending, before recording",
    )
    .await?;
    expect_equal(
        &h.nested(None, None)
            .await?
            .deliver_cancels("p", Arc::clone(&send_fn))
            .await?,
        &1usize,
        "the restart re-sends it",
    )?;
    expect_equal(
        &h.nested(None, None)
            .await?
            .deliver_cancels("p", Arc::clone(&send_fn))
            .await?,
        &0usize,
        "recorded: never sent again",
    )?;
    assert_eq_val(
        &*sent.lock().unwrap_or_else(|p| p.into_inner()),
        &vec![cancel_target_of("c1", "e2"), cancel_target_of("c1", "e2")],
        "sent until recorded",
    )?;
    Ok(())
}

pub fn kill_nest_cases() -> BTreeMap<&'static str, KillNestCase> {
    let mut map: BTreeMap<&'static str, KillNestCase> = BTreeMap::new();
    map.insert("KILL-NEST-01", Arc::new(|a| Box::pin(before_admission(a))));
    map.insert("KILL-NEST-02", Arc::new(|a| Box::pin(after_admission(a))));
    map.insert("KILL-NEST-03", Arc::new(|a| Box::pin(around_cascade(a))));
    map
}

pub const KILL_NEST_CASES: fn() -> BTreeMap<&'static str, KillNestCase> = kill_nest_cases;
