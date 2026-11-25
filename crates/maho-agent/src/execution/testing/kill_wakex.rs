//! Port of senpi `packages/agent/src/execution/testing/kill-wakex.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::wake_types::WakeEvent;

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::{BrainKill, BrainKillAt, batch_harness, expect_rejects_msg};
use super::wake_exec_cases::{ids, link_to, scope_owner_lookup, wake_of};

type KillWakeCase = CaseImplementation<BatchCaseAdapters>;

async fn before_offer(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let dying = h
        .wake_outbox(
            "e1",
            Some(BrainKill {
                label: "offer".to_string(),
                at: BrainKillAt::Before,
            }),
        )
        .await?;
    expect_rejects_msg(
        async move {
            dying
                .offer(wake_of(1, "s"))
                .await
                .map_err(|e| e.to_string())
        },
        "brain killed around",
        "the dying offer",
    )
    .await?;
    expect_equal(
        &h.wake_outbox("e1", None).await?.pending().await?,
        &Vec::<WakeEvent>::new(),
        "nothing durable",
    )?;
    Ok(())
}

async fn after_offer(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let dying = h
        .wake_outbox(
            "e1",
            Some(BrainKill {
                label: "offer".to_string(),
                at: BrainKillAt::After,
            }),
        )
        .await?;
    expect_rejects_msg(
        async move {
            dying
                .offer(wake_of(1, "s"))
                .await
                .map_err(|e| e.to_string())
        },
        "brain killed around",
        "the dying offer",
    )
    .await?;
    let outbox = h.wake_outbox("e1", None).await?;
    let inbox = h.wake_inbox(scope_owner_lookup(), None).await?;
    expect_equal(
        &outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?,
        &1usize,
        "re-offered after the restart",
    )?;
    expect_equal(
        &inbox.read().await?.pending.len(),
        &1usize,
        "delivered once",
    )?;
    expect_equal(&outbox.pending().await?, &Vec::<WakeEvent>::new(), "acked")?;
    Ok(())
}

async fn before_commit(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let outbox = h.wake_outbox("e1", None).await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    let dying = h
        .wake_inbox(
            scope_owner_lookup(),
            Some(BrainKill {
                label: format!("commit:{}", wake.event_id),
                at: BrainKillAt::Before,
            }),
        )
        .await?;
    expect_rejects_msg(
        outbox.reoffer(link_to(dying, "e1")),
        "brain killed around",
        "the dying commit",
    )
    .await?;
    expect_equal(
        &ids(&outbox.pending().await?),
        &vec![wake.event_id.clone()],
        "never acked",
    )?;
    let inbox = h.wake_inbox(scope_owner_lookup(), None).await?;
    outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?;
    expect_equal(
        &ids(&inbox.read().await?.pending),
        &vec![wake.event_id],
        "committed once",
    )?;
    expect_equal(
        &outbox.pending().await?,
        &Vec::<WakeEvent>::new(),
        "acked after the commit",
    )?;
    Ok(())
}

async fn after_commit(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let outbox = h.wake_outbox("e1", None).await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    let dying = h
        .wake_inbox(
            scope_owner_lookup(),
            Some(BrainKill {
                label: format!("commit:{}", wake.event_id),
                at: BrainKillAt::After,
            }),
        )
        .await?;
    expect_rejects_msg(
        outbox.reoffer(link_to(dying, "e1")),
        "brain killed around",
        "the dying ack",
    )
    .await?;
    let inbox = h.wake_inbox(scope_owner_lookup(), None).await?;
    outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?;
    expect_equal(
        &ids(&inbox.read().await?.pending),
        &vec![wake.event_id],
        "one wake on the brain",
    )?;
    expect_equal(
        &outbox.pending().await?,
        &Vec::<WakeEvent>::new(),
        "the duplicate's ack removed it",
    )?;
    Ok(())
}

async fn before_ack_record(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let offered = h
        .wake_outbox("e1", None)
        .await?
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    let inbox = h.wake_inbox(scope_owner_lookup(), None).await?;
    expect_rejects_msg(
        h.wake_outbox(
            "e1",
            Some(BrainKill {
                label: "ack".to_string(),
                at: BrainKillAt::Before,
            }),
        )
        .await?
        .reoffer(link_to(Arc::clone(&inbox), "e1")),
        "brain killed around",
        "the dying ack record",
    )
    .await?;
    let outbox = h.wake_outbox("e1", None).await?;
    expect_equal(
        &ids(&outbox.pending().await?),
        &vec![offered.event_id.clone()],
        "still pending on the hand",
    )?;
    outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?;
    expect_equal(
        &outbox.pending().await?,
        &Vec::<WakeEvent>::new(),
        "acked by the duplicate",
    )?;
    expect_equal(
        &ids(&inbox.read().await?.pending),
        &vec![offered.event_id],
        "never a second wake",
    )?;
    Ok(())
}

async fn before_take_record(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let outbox = h.wake_outbox("e1", None).await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    outbox
        .reoffer(link_to(
            h.wake_inbox(scope_owner_lookup(), None).await?,
            "e1",
        ))
        .await?;
    let taken = Arc::new(Mutex::new(Vec::new()));
    let make_consume =
        |t: Arc<Mutex<Vec<WakeEvent>>>| -> crate::execution::wake_inbox::WakeConsumeFn {
            Arc::new(move |got| {
                let t_inner = Arc::clone(&t);
                Box::pin(async move {
                    t_inner.lock().unwrap_or_else(|p| p.into_inner()).push(got);
                    Ok(())
                })
            })
        };
    expect_rejects_msg(
        h.wake_inbox(
            scope_owner_lookup(),
            Some(BrainKill {
                label: format!("take:{}", wake.event_id),
                at: BrainKillAt::Before,
            }),
        )
        .await?
        .drain(make_consume(Arc::clone(&taken))),
        "brain killed around",
        "the dying take",
    )
    .await?;
    expect_equal(
        &h.wake_inbox(scope_owner_lookup(), None)
            .await?
            .drain(make_consume(Arc::clone(&taken)))
            .await?,
        &1usize,
        "handed over again after the restart",
    )?;
    let taken_ids = ids(&taken.lock().unwrap_or_else(|p| p.into_inner()));
    expect_equal(
        &taken_ids,
        &vec![wake.event_id.clone(), wake.event_id],
        "the same wake, by the same id",
    )?;
    expect_equal(
        &h.wake_inbox(scope_owner_lookup(), None)
            .await?
            .drain(Arc::new(|_| Box::pin(async { Ok(()) })))
            .await?,
        &0usize,
        "then recorded",
    )?;
    Ok(())
}

pub fn kill_wakex_cases() -> BTreeMap<&'static str, KillWakeCase> {
    let mut map: BTreeMap<&'static str, KillWakeCase> = BTreeMap::new();
    map.insert("KILL-WAKEX-01", Arc::new(|a| Box::pin(before_offer(a))));
    map.insert("KILL-WAKEX-02", Arc::new(|a| Box::pin(after_offer(a))));
    map.insert("KILL-WAKEX-03", Arc::new(|a| Box::pin(before_commit(a))));
    map.insert("KILL-WAKEX-04", Arc::new(|a| Box::pin(after_commit(a))));
    map.insert(
        "KILL-WAKEX-05",
        Arc::new(|a| Box::pin(before_ack_record(a))),
    );
    map.insert(
        "KILL-WAKEX-06",
        Arc::new(|a| Box::pin(before_take_record(a))),
    );
    map
}

pub const KILL_WAKEX_CASES: fn() -> BTreeMap<&'static str, KillWakeCase> = kill_wakex_cases;
