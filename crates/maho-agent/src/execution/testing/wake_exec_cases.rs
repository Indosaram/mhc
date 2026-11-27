//! Port of senpi `packages/agent/src/execution/testing/wake-exec-cases.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::testing::store_harness::expect_equal;
use crate::execution::wake_inbox::{OwnerLookup, WakeInbox};
use crate::execution::wake_outbox::{ReofferSendFn, WakeOutbox};
use crate::execution::wake_types::{
    WAKE_RETENTION_MS, WakeEvent, WakeInboxRecord, WakeOffer, WakeReceipt, WakeRefusal,
};

use super::batch_executor::BatchCaseAdapters;
use super::batch_harness::{BatchHarness, batch_harness};

type WakeCase = CaseImplementation<BatchCaseAdapters>;

pub fn scope_owner(scope: &str) -> Option<String> {
    if scope == "s" {
        Some("e1".to_string())
    } else {
        None
    }
}

pub fn scope_owner_lookup() -> OwnerLookup {
    Arc::new(scope_owner)
}

pub fn wake_of(n: i64, scope: &str) -> WakeOffer {
    WakeOffer {
        scope: scope.to_string(),
        source: "terminal".to_string(),
        payload: json!({ "n": n }),
    }
}

pub fn link_to(inbox: Arc<dyn WakeInbox>, hand: &str) -> ReofferSendFn {
    let hand = hand.to_string();
    Arc::new(move |wake| {
        let inbox = Arc::clone(&inbox);
        let hand = hand.clone();
        Box::pin(async move { inbox.receive(&hand, wake, None).await })
    })
}

async fn wake_pair(h: &BatchHarness) -> Result<(Arc<dyn WakeOutbox>, Arc<dyn WakeInbox>), String> {
    Ok((
        h.wake_outbox("e1", None).await?,
        h.wake_inbox(scope_owner_lookup(), None).await?,
    ))
}

pub fn ids(wakes: &[WakeEvent]) -> Vec<String> {
    wakes.iter().map(|w| w.event_id.clone()).collect()
}

async fn delivery(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let (outbox, inbox) = wake_pair(&h).await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &ids(&outbox.pending().await?),
        &vec![wake.event_id.clone()],
        "durable on the hand before it is sent",
    )?;
    expect_equal(
        &outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?,
        &1usize,
        "sent",
    )?;
    expect_equal(
        &outbox.pending().await?,
        &Vec::<WakeEvent>::new(),
        "removed by its ack",
    )?;
    let taken = Arc::new(Mutex::new(Vec::new()));
    let taken_c = Arc::clone(&taken);
    let count = inbox
        .drain(Arc::new(move |got| {
            let taken_inner = Arc::clone(&taken_c);
            Box::pin(async move {
                taken_inner
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(got);
                Ok(())
            })
        }))
        .await?;
    expect_equal(&count, &1usize, "taken")?;
    expect_equal(
        &*taken.lock().unwrap_or_else(|p| p.into_inner()),
        &vec![wake],
        "the loop gets the wake once",
    )?;
    expect_equal(
        &inbox
            .drain(Arc::new(|_| Box::pin(async { Ok(()) })))
            .await?,
        &0usize,
        "and never again",
    )?;
    Ok(())
}

async fn dedup(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let (outbox, inbox) = wake_pair(&h).await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &inbox.receive("e1", wake.clone(), None).await?,
        &WakeReceipt::Ack {
            event_id: wake.event_id.clone(),
            duplicate: false,
        },
        "committed",
    )?;
    expect_equal(
        &ids(&outbox.pending().await?),
        &vec![wake.event_id.clone()],
        "the ack was lost",
    )?;
    outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?;
    expect_equal(
        &outbox.pending().await?,
        &Vec::<WakeEvent>::new(),
        "the duplicate's ack removes it",
    )?;
    expect_equal(
        &ids(&inbox.read().await?.pending),
        &vec![wake.event_id],
        "one wake on the brain",
    )?;
    Ok(())
}

async fn reconnect(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let (outbox, inbox) = wake_pair(&h).await?;
    let offered = vec![
        outbox
            .offer(wake_of(1, "s"))
            .await
            .map_err(|e| e.to_string())?,
        outbox
            .offer(wake_of(2, "s"))
            .await
            .map_err(|e| e.to_string())?,
        outbox
            .offer(wake_of(3, "s"))
            .await
            .map_err(|e| e.to_string())?,
    ];
    let restarted = h.wake_outbox("e1", None).await?;
    expect_equal(
        &restarted.reoffer(link_to(Arc::clone(&inbox), "e1")).await?,
        &3usize,
        "all re-offered after a restart",
    )?;
    expect_equal(
        &ids(&inbox.read().await?.pending),
        &ids(&offered),
        "in offer order",
    )?;
    Ok(())
}

async fn binding(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let (outbox, inbox) = wake_pair(&h).await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &inbox.receive("e2", wake.clone(), None).await?,
        &WakeReceipt::Refused {
            event_id: wake.event_id.clone(),
            reason: WakeRefusal::NotScopeHand,
        },
        "another hand",
    )?;
    let foreign = outbox
        .offer(wake_of(2, "elsewhere"))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &inbox.receive("e1", foreign.clone(), None).await?,
        &WakeReceipt::Refused {
            event_id: foreign.event_id.clone(),
            reason: WakeRefusal::UnknownScope,
        },
        "a scope this brain does not own",
    )?;
    expect_equal(
        &inbox.read().await?,
        &WakeInboxRecord::default(),
        "nothing committed",
    )?;
    outbox
        .receipt(WakeReceipt::Refused {
            event_id: wake.event_id.clone(),
            reason: WakeRefusal::NotScopeHand,
        })
        .await?;
    outbox
        .receipt(WakeReceipt::Refused {
            event_id: foreign.event_id.clone(),
            reason: WakeRefusal::UnknownScope,
        })
        .await?;
    expect_equal(
        &ids(&outbox.pending().await?),
        &vec![wake.event_id.clone(), foreign.event_id.clone()],
        "a refusal is never an ack",
    )?;
    let resent = Arc::new(Mutex::new(Vec::new()));
    let resent_c = Arc::clone(&resent);
    let inbox_c = Arc::clone(&inbox);
    let reoffered = outbox
        .reoffer(Arc::new(move |sent| {
            let resent_inner = Arc::clone(&resent_c);
            let inbox_inner = Arc::clone(&inbox_c);
            Box::pin(async move {
                resent_inner
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(sent.event_id.clone());
                inbox_inner.receive("e2", sent, None).await
            })
        }))
        .await?;
    expect_equal(
        &reoffered,
        &1usize,
        "a wake refused for good is never re-offered; an unknown scope is retried",
    )?;
    expect_equal(
        &*resent.lock().unwrap_or_else(|p| p.into_inner()),
        &vec![foreign.event_id],
        "only the unknown-scope wake is sent again",
    )?;
    outbox.close_scope("s").await?;
    outbox.close_scope("elsewhere").await?;
    h.advance(WAKE_RETENTION_MS + 1);
    expect_equal(
        &outbox.sweep().await?.len(),
        &2usize,
        "refused wakes still expire with their scope",
    )?;
    Ok(())
}

async fn retention(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let outbox = h.wake_outbox("e1", None).await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &outbox.sweep().await?,
        &Vec::<String>::new(),
        "an open scope never expires",
    )?;
    outbox.close_scope("s").await?;
    h.advance(WAKE_RETENTION_MS);
    expect_equal(
        &outbox.sweep().await?,
        &Vec::<String>::new(),
        "kept for the whole retention",
    )?;
    h.advance(1);
    expect_equal(
        &outbox.sweep().await?,
        &vec![wake.event_id.clone()],
        "dropped after it",
    )?;
    expect_equal(&outbox.pending().await?, &Vec::<WakeEvent>::new(), "gone")?;
    let logged = h.wake_log("e1").await?;
    expect_equal(
        &logged,
        &vec![json!({
            "reason": "wake_expired",
            "eventId": wake.event_id,
            "scope": "s",
        })],
        "logged as wake_expired",
    )?;
    Ok(())
}

async fn brain_retention(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let (outbox, inbox) = wake_pair(&h).await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &inbox.receive("e1", wake.clone(), None).await?,
        &WakeReceipt::Ack {
            event_id: wake.event_id.clone(),
            duplicate: false,
        },
        "committed",
    )?;
    inbox.close_scope("s").await?;
    expect_equal(
        &inbox.receive("e1", wake.clone(), None).await?,
        &WakeReceipt::Ack {
            event_id: wake.event_id.clone(),
            duplicate: true,
        },
        "a duplicate",
    )?;
    let late = outbox
        .offer(wake_of(2, "s"))
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &inbox.receive("e1", late.clone(), None).await?,
        &WakeReceipt::Refused {
            event_id: late.event_id,
            reason: WakeRefusal::ScopeClosed,
        },
        "a new wake for a closed scope",
    )?;
    expect_equal(
        &inbox.sweep().await?,
        &Vec::<String>::new(),
        "kept for the whole retention",
    )?;
    h.advance(WAKE_RETENTION_MS + 1);
    expect_equal(
        &inbox.sweep().await?,
        &vec!["s".to_string()],
        "the seen ids are dropped after it",
    )?;
    expect_equal(
        &inbox.receive("e1", wake.clone(), None).await?,
        &WakeReceipt::Refused {
            event_id: wake.event_id.clone(),
            reason: WakeRefusal::ScopeClosed,
        },
        "a late re-offer is refused, never committed again",
    )?;
    expect_equal(
        &ids(&inbox.read().await?.pending),
        &vec![wake.event_id],
        "one wake on the brain",
    )?;
    expect_equal(
        &inbox.sweep().await?,
        &Vec::<String>::new(),
        "a scope is dropped once",
    )?;
    Ok(())
}

async fn before_binding(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let bound = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bound_c = Arc::clone(&bound);
    let outbox = h.wake_outbox("e1", None).await?;
    let inbox = h
        .wake_inbox(
            Arc::new(move |scope| {
                if bound_c.load(std::sync::atomic::Ordering::SeqCst) && scope == "s" {
                    Some("e1".to_string())
                } else {
                    None
                }
            }),
            None,
        )
        .await?;
    let wake = outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?;
    expect_equal(
        &ids(&outbox.pending().await?),
        &vec![wake.event_id.clone()],
        "still pending on the hand",
    )?;
    bound.store(true, std::sync::atomic::Ordering::SeqCst);
    expect_equal(
        &outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?,
        &1usize,
        "re-offered once the scope is bound",
    )?;
    expect_equal(&outbox.pending().await?, &Vec::<WakeEvent>::new(), "acked")?;
    expect_equal(
        &ids(&inbox.read().await?.pending),
        &vec![wake.event_id],
        "delivered once",
    )?;
    Ok(())
}

async fn concurrent_drains(adapters: BatchCaseAdapters) -> ConformanceResult {
    let h = batch_harness(adapters).await?;
    let (outbox, inbox) = wake_pair(&h).await?;
    outbox
        .offer(wake_of(1, "s"))
        .await
        .map_err(|e| e.to_string())?;
    outbox
        .offer(wake_of(2, "s"))
        .await
        .map_err(|e| e.to_string())?;
    outbox.reoffer(link_to(Arc::clone(&inbox), "e1")).await?;
    let taken = Arc::new(Mutex::new(Vec::new()));
    let make_consume = |t: Arc<Mutex<Vec<String>>>| -> crate::execution::wake_inbox::WakeConsumeFn {
        Arc::new(move |wake: WakeEvent| {
            let t_inner = Arc::clone(&t);
            Box::pin(async move {
                t_inner
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(wake.event_id);
                Ok(())
            })
        })
    };
    let (first_res, second_res) = tokio::join!(
        inbox.drain(make_consume(Arc::clone(&taken))),
        inbox.drain(make_consume(Arc::clone(&taken)))
    );
    let first = first_res?;
    let second = second_res?;
    expect_equal(&(first + second), &2usize, "two wakes taken in all")?;
    let guard = taken.lock().unwrap_or_else(|p| p.into_inner());
    let uniq: BTreeSet<&String> = guard.iter().collect();
    expect_equal(
        &(uniq.len() == guard.len() && guard.len() == 2),
        &true,
        "each wake once",
    )?;
    Ok(())
}

pub fn wake_exec_cases() -> BTreeMap<&'static str, WakeCase> {
    let mut map: BTreeMap<&'static str, WakeCase> = BTreeMap::new();
    map.insert("WAKEX-01", Arc::new(|a| Box::pin(delivery(a))));
    map.insert("WAKEX-02", Arc::new(|a| Box::pin(dedup(a))));
    map.insert("WAKEX-03", Arc::new(|a| Box::pin(reconnect(a))));
    map.insert("WAKEX-04", Arc::new(|a| Box::pin(binding(a))));
    map.insert("WAKEX-05", Arc::new(|a| Box::pin(retention(a))));
    map.insert("WAKEX-06", Arc::new(|a| Box::pin(brain_retention(a))));
    map.insert("WAKEX-07", Arc::new(|a| Box::pin(concurrent_drains(a))));
    map.insert("WAKEX-08", Arc::new(|a| Box::pin(before_binding(a))));
    map
}

pub const WAKE_EXEC_CASES: fn() -> BTreeMap<&'static str, WakeCase> = wake_exec_cases;
