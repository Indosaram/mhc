//! Port of senpi `packages/agent/src/durable/testing/wake-cases.ts`.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::DurableStoreError;
use crate::durable::wake_host::Wake;

use super::store_harness::expect_equal;
use super::wake_harness::{BootOptions, HEARTBEAT_MS, WakeCaseAdapters, create_slot, restart_host};

type WakeCase = CaseImplementation<WakeCaseAdapters>;

const TEN_MINUTES: i64 = 600_000;

async fn pre_arm(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = create_slot(adapters, 1).await?;
    let session = slot
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.trace(),
        &vec![
            format!("set:+{HEARTBEAT_MS}"),
            "resume".to_owned(),
            "clear".to_owned(),
        ],
        "start arms a fallback before it resumes",
    )?;

    slot.clear_trace();
    let slot_for_work = slot.clone();
    session
        .host
        .admit(|| async move {
            let at = slot_for_work
                .alarm_at()
                .await
                .map_err(DurableStoreError::Other)?;
            if let Some(val) = at {
                slot_for_work.push_trace(format!("work alarm=+{val}"));
            }
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.trace(),
        &vec![
            format!("set:+{HEARTBEAT_MS}"),
            format!("work alarm=+{HEARTBEAT_MS}"),
            "clear".to_owned(),
        ],
        "admit arms durably before work starts, and arms from the idle loop's wake when the work ends",
    )?;

    let failing: Result<(), DurableStoreError> = session
        .host
        .admit(|| async move { Err(DurableStoreError::Other("work failed".to_owned())) })
        .await;
    if failing.is_ok() {
        return Err("a failing admission resolved".to_owned());
    }
    expect_equal(
        &slot.alarm_at().await?,
        &Some(HEARTBEAT_MS),
        "work that fails leaves the recovery alarm armed",
    )?;

    let loop_handle = session.loop_handle.clone();
    let target_at = slot.base + TEN_MINUTES;
    session
        .host
        .admit(|| async move { loop_handle.schedule("late", target_at).await })
        .await
        .map_err(|e| e.to_string())?;
    slot.kill();
    slot.clear_trace();
    slot.instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.trace(),
        &vec![
            format!("set:+{HEARTBEAT_MS}"),
            "resume".to_owned(),
            format!("set:+{TEN_MINUTES}"),
        ],
        "a new instance lowers a later alarm to the fallback before resuming",
    )?;

    let inst = slot
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    inst.host
        .update(Wake {
            active: false,
            at: Some(slot.base + 30_000),
        })
        .await
        .map_err(|e| e.to_string())?;
    slot.kill();
    slot.clear_trace();
    slot.instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.trace(),
        &vec!["resume".to_owned(), format!("set:+{TEN_MINUTES}")],
        "an earlier alarm is kept as the fallback",
    )
}

async fn update(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = create_slot(adapters, 1).await?;
    let session = slot
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    let schedule = |id: &'static str, offset: i64| {
        let host = session.host.clone();
        let loop_handle = session.loop_handle.clone();
        let at = slot.base + offset;
        async move {
            host.admit(|| async move { loop_handle.schedule(id, at).await })
                .await
        }
    };

    slot.clear_trace();
    schedule("a", TEN_MINUTES)
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.trace(),
        &vec![
            format!("set:+{HEARTBEAT_MS}"),
            format!("set:+{HEARTBEAT_MS}"),
            format!("set:+{TEN_MINUTES}"),
        ],
        "the commit's update keeps the heartbeat while the admission is in flight; after it, the alarm is the wake",
    )?;
    schedule("b", 300_000).await.map_err(|e| e.to_string())?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(300_000i64),
        "an earlier wake lowers the alarm",
    )?;
    schedule("c", 900_000).await.map_err(|e| e.to_string())?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(300_000i64),
        "a later wake never raises it past the earliest",
    )?;

    session
        .host
        .update(Wake {
            active: true,
            at: Some(slot.base + 300_000),
        })
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(HEARTBEAT_MS),
        "while active, a heartbeat earlier than the wake",
    )?;
    session
        .host
        .update(Wake {
            active: true,
            at: Some(slot.base + 30_000),
        })
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(30_000i64),
        "never later than the earliest wake",
    )?;
    session
        .host
        .update(Wake {
            active: false,
            at: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.alarm_at().await?,
        &Option::<i64>::None,
        "cleared only when the loop is idle",
    )?;
    expect_equal(
        &slot.ran().await?,
        &Vec::<String>::new(),
        "updates run nothing",
    )
}

async fn resume(adapters: WakeCaseAdapters) -> ConformanceResult {
    let wake_adapter = Arc::clone(&adapters.wake);
    let slot = create_slot(adapters, 1).await?;
    let session = slot
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    for (id, offset) in [("a", 120_000), ("b", 120_000), ("c", TEN_MINUTES)] {
        let loop_handle = session.loop_handle.clone();
        let at = slot.base + offset;
        session
            .host
            .admit(|| async move { loop_handle.schedule(id, at).await })
            .await
            .map_err(|e| e.to_string())?;
    }

    slot.clear_trace();
    wake_adapter.advance_to(slot.base + 120_000).await?;
    expect_equal(&slot.fires(), &1usize, "one fire at the wake")?;
    let trace = slot.trace();
    let head_slice: Vec<String> = trace.into_iter().take(2).collect();
    expect_equal(
        &head_slice,
        &vec![
            format!("set:+{}", 120_000 + HEARTBEAT_MS),
            "resume".to_owned(),
        ],
        "a fire re-arms before resume",
    )?;
    expect_equal(
        &slot.ran().await?,
        &vec!["a", "b"],
        "the due tasks run once, in order",
    )?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(TEN_MINUTES),
        "then the alarm is the next wake",
    )?;

    session
        .host
        .update(Wake {
            active: true,
            at: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    wake_adapter
        .advance_to(slot.base + 120_000 + HEARTBEAT_MS)
        .await?;
    expect_equal(
        &slot.ran().await?,
        &vec!["a", "b"],
        "an early alarm runs nothing",
    )?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(TEN_MINUTES),
        "and re-arms from the loop's wake",
    )?;

    wake_adapter.advance_to(slot.base + TEN_MINUTES).await?;
    expect_equal(
        &slot.ran().await?,
        &vec!["a", "b", "c"],
        "the last task runs at its wake",
    )?;
    expect_equal(
        &slot.alarm_at().await?,
        &Option::<i64>::None,
        "an idle loop leaves no alarm",
    )?;
    expect_equal(&slot.fires(), &3usize, "no fire beyond the alarms armed")
}

async fn retention(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = create_slot(adapters.clone(), 1).await?;
    let session = slot
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    let loop_handle = session.loop_handle.clone();
    let at = slot.base + TEN_MINUTES;
    session
        .host
        .admit(|| async move { loop_handle.schedule("a", at).await })
        .await
        .map_err(|e| e.to_string())?;

    restart_host(&adapters, std::slice::from_ref(&slot)).await?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(TEN_MINUTES),
        "the alarm is durable across a host restart",
    )?;
    slot.instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    slot.kill();
    slot.instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(TEN_MINUTES),
        "reopened instances keep it",
    )?;

    adapters
        .wake
        .advance_to(slot.base + TEN_MINUTES - 1)
        .await?;
    expect_equal(
        &slot.fires(),
        &0usize,
        "nothing fires early, whatever instances came and went",
    )?;
    adapters.wake.advance_to(slot.base + TEN_MINUTES).await?;
    expect_equal(&slot.fires(), &1usize, "it fires once")?;
    expect_equal(&slot.ran().await?, &vec!["a"], "the task runs once")?;
    expect_equal(
        &slot.alarm_at().await?,
        &Option::<i64>::None,
        "and the alarm is not retained after the work",
    )?;
    adapters
        .wake
        .advance_to(slot.base + 3 * TEN_MINUTES)
        .await?;
    expect_equal(&slot.fires(), &1usize, "a consumed alarm never fires again")
}

pub fn wake_cases() -> BTreeMap<&'static str, WakeCase> {
    let mut map: BTreeMap<&'static str, WakeCase> = BTreeMap::new();
    map.insert("WAKE-01", Arc::new(|a| Box::pin(pre_arm(a))));
    map.insert("WAKE-02", Arc::new(|a| Box::pin(update(a))));
    map.insert("WAKE-03", Arc::new(|a| Box::pin(resume(a))));
    map.insert("WAKE-RET-01", Arc::new(|a| Box::pin(retention(a))));
    map
}

pub const WAKE_CASES: fn() -> BTreeMap<&'static str, WakeCase> = wake_cases;
