//! Port of senpi `packages/agent/src/durable/testing/kill-wake.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::conformance::runner::{CaseImplementation, ConformanceResult};
use crate::durable::store::{DurableStoreError, StoreOperation};
use crate::durable::wake_host::{WakeAlarmStage, WakeOp};

use super::store_harness::expect_equal;
use super::wake_harness::{
    BootOptions, HEARTBEAT_MS, SessionInstance, SessionSlot, WakeCaseAdapters, create_slot,
    cut_alarm, cut_store, cut_wake, expect_wake_cut,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillWakeOperation {
    Admit,
    Commit,
    Update,
    Fire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillWakeDeclaration {
    pub id: &'static str,
    pub operation: KillWakeOperation,
    pub point: &'static str,
    pub placement: &'static str,
}

const LATER: i64 = 600_000;
const DUE: i64 = 120_000;

async fn killed_admission(
    adapters: WakeCaseAdapters,
    cut: BootOptions,
) -> Result<SessionSlot, String> {
    let slot = create_slot(adapters, 1).await?;
    let inst = slot.instance(cut).await.map_err(|e| e.to_string())?;
    let target_at = slot.base + LATER;
    let loop_handle = inst.loop_handle;
    let res = inst
        .host
        .admit(|| async move { loop_handle.schedule("a", target_at).await })
        .await;
    expect_wake_cut(res, "the killed admission").await?;
    slot.kill();
    Ok(slot)
}

async fn recovers_through_heartbeat(
    adapters: &WakeCaseAdapters,
    slot: &SessionSlot,
) -> ConformanceResult {
    expect_equal(
        &slot.alarm_at().await?,
        &Some(HEARTBEAT_MS),
        "the pre-armed heartbeat survived the kill",
    )?;
    adapters.wake.advance_to(slot.base + HEARTBEAT_MS).await?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(LATER),
        "the heartbeat's instance armed the committed wake",
    )?;
    adapters.wake.advance_to(slot.base + LATER).await?;
    expect_equal(
        &slot.ran().await?,
        &vec!["a"],
        "the committed task ran exactly once",
    )?;
    expect_equal(
        &slot.alarm_at().await?,
        &Option::<i64>::None,
        "and the alarm is cleared",
    )
}

async fn killed_fire(adapters: WakeCaseAdapters, cut: BootOptions) -> Result<SessionSlot, String> {
    let wake_adapter = Arc::clone(&adapters.wake);
    let slot = create_slot(adapters, 1).await?;
    let inst = slot.instance(cut).await.map_err(|e| e.to_string())?;
    let target_at = slot.base + DUE;
    let loop_handle = inst.loop_handle;
    inst.host
        .admit(|| async move { loop_handle.schedule("a", target_at).await })
        .await
        .map_err(|e| e.to_string())?;
    wake_adapter.advance_to(slot.base + DUE).await?;
    expect_equal(
        &slot.fires(),
        &1usize,
        "the alarm fired into the instance that is killed",
    )?;
    Ok(slot)
}

async fn recovers_after_killed_fire(
    adapters: &WakeCaseAdapters,
    slot: &SessionSlot,
) -> ConformanceResult {
    expect_equal(
        &slot.alarm_at().await?,
        &Some(DUE + HEARTBEAT_MS),
        "the fire's re-arm survived the kill",
    )?;
    adapters
        .wake
        .advance_to(slot.base + DUE + HEARTBEAT_MS)
        .await?;
    expect_equal(&slot.ran().await?, &vec!["a"], "the task ran exactly once")?;
    expect_equal(
        &slot.alarm_at().await?,
        &Option::<i64>::None,
        "and the alarm is cleared",
    )?;
    adapters.wake.advance_to(slot.base + LATER).await?;
    expect_equal(&slot.fires(), &2usize, "no fire beyond the recovery")
}

async fn admission_during_fire(adapters: WakeCaseAdapters) -> Result<SessionSlot, String> {
    let fire_done = Arc::new(Notify::new());
    let fire_done_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    type AdmissionHandle =
        Arc<Mutex<Option<tokio::task::JoinHandle<Result<(), DurableStoreError>>>>>;
    let admission_handle: AdmissionHandle = Arc::new(Mutex::new(None));
    let session_cell: Arc<Mutex<Option<SessionInstance>>> = Arc::new(Mutex::new(None));

    let slot = create_slot(adapters.clone(), 2).await?;
    let slot_base = slot.base;

    let session_for_hook = Arc::clone(&session_cell);
    let handle_for_hook = Arc::clone(&admission_handle);
    let fire_done_for_hook = Arc::clone(&fire_done);
    let fire_flag_for_hook = Arc::clone(&fire_done_flag);

    let first = slot
        .instance(BootOptions {
            store: cut_store(StoreOperation::Commit, "durable", 3),
            on_wake_point: Some(Arc::new(move |point| {
                if point.op != WakeOp::Fire {
                    return Ok(());
                }
                let Some(sess) = session_for_hook
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
                else {
                    return Ok(());
                };
                let mut guard = handle_for_hook.lock().unwrap_or_else(|p| p.into_inner());
                if guard.is_some() {
                    return Ok(());
                }
                let host = sess.host;
                let loop_handle = sess.loop_handle;
                let notify = Arc::clone(&fire_done_for_hook);
                let flag = Arc::clone(&fire_flag_for_hook);
                let target_at = slot_base + LATER;
                let jh = tokio::spawn(async move {
                    host.admit(|| async move {
                        while !flag.load(std::sync::atomic::Ordering::SeqCst) {
                            notify.notified().await;
                        }
                        loop_handle.schedule("b", target_at).await
                    })
                    .await
                });
                *guard = Some(jh);
                Ok(())
            })),
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    *session_cell.lock().unwrap_or_else(|p| p.into_inner()) = Some(first.clone());

    let loop_a = first.loop_handle.clone();
    let target_a = slot.base + DUE;
    first
        .host
        .admit(|| async move { loop_a.schedule("a", target_a).await })
        .await
        .map_err(|e| e.to_string())?;
    adapters.wake.advance_to(slot.base + DUE).await?;
    expect_equal(
        &slot.ran().await?,
        &vec!["a"],
        "the fire resumed its due task",
    )?;

    let jh = admission_handle
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .ok_or_else(|| "the admission did not start during the fire".to_owned())?;
    fire_done_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    fire_done.notify_waiters();
    let admit_res = jh
        .await
        .map_err(|e| format!("admission task panicked: {e}"))?;
    expect_wake_cut(admit_res, "the admission killed after its commit").await?;
    slot.kill();
    Ok(slot)
}

async fn kill_wake_01(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = killed_admission(
        adapters.clone(),
        BootOptions {
            alarm: cut_alarm("set", WakeAlarmStage::Before, 2),
            ..Default::default()
        },
    )
    .await?;
    expect_equal(
        &slot.alarm_at().await?,
        &Option::<i64>::None,
        "nothing of the admission is durable",
    )?;
    let inst = slot
        .instance(BootOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    let target_at = slot.base + LATER;
    let loop_handle = inst.loop_handle;
    inst.host
        .admit(|| async move { loop_handle.schedule("a", target_at).await })
        .await
        .map_err(|e| e.to_string())?;
    adapters.wake.advance_to(slot.base + LATER).await?;
    expect_equal(
        &slot.ran().await?,
        &vec!["a"],
        "the retried admission runs once",
    )
}

async fn kill_wake_02(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = killed_admission(
        adapters.clone(),
        BootOptions {
            on_wake_point: Some(cut_wake(WakeOp::Admit, 1)),
            ..Default::default()
        },
    )
    .await?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(HEARTBEAT_MS),
        "the arm is durable",
    )?;
    adapters.wake.advance_to(slot.base + HEARTBEAT_MS).await?;
    expect_equal(
        &slot.ran().await?,
        &Vec::<String>::new(),
        "an alarm without work runs nothing",
    )?;
    expect_equal(
        &slot.alarm_at().await?,
        &Option::<i64>::None,
        "and ends cleared",
    )?;
    expect_equal(&slot.fires(), &1usize, "one harmless fire")
}

async fn kill_wake_03(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = killed_admission(
        adapters.clone(),
        BootOptions {
            store: cut_store(StoreOperation::Commit, "durable", 1),
            ..Default::default()
        },
    )
    .await?;
    recovers_through_heartbeat(&adapters, &slot).await
}

async fn kill_wake_04(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = killed_admission(
        adapters.clone(),
        BootOptions {
            alarm: cut_alarm("set", WakeAlarmStage::Before, 3),
            ..Default::default()
        },
    )
    .await?;
    recovers_through_heartbeat(&adapters, &slot).await
}

async fn kill_wake_05(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = killed_fire(
        adapters.clone(),
        BootOptions {
            on_wake_point: Some(cut_wake(WakeOp::Fire, 1)),
            ..Default::default()
        },
    )
    .await?;
    expect_equal(
        &slot.ran().await?,
        &Vec::<String>::new(),
        "nothing ran before the kill",
    )?;
    recovers_after_killed_fire(&adapters, &slot).await
}

async fn kill_wake_06(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = killed_fire(
        adapters.clone(),
        BootOptions {
            store: cut_store(StoreOperation::Commit, "durable", 2),
            ..Default::default()
        },
    )
    .await?;
    expect_equal(
        &slot.ran().await?,
        &vec!["a"],
        "the task's commit is durable",
    )?;
    recovers_after_killed_fire(&adapters, &slot).await
}

async fn kill_wake_07(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = killed_fire(
        adapters.clone(),
        BootOptions {
            alarm: cut_alarm("clear", WakeAlarmStage::Before, 2),
            ..Default::default()
        },
    )
    .await?;
    expect_equal(&slot.ran().await?, &vec!["a"], "the work is durable")?;
    recovers_after_killed_fire(&adapters, &slot).await
}

async fn kill_wake_08(adapters: WakeCaseAdapters) -> ConformanceResult {
    let slot = admission_during_fire(adapters.clone()).await?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(DUE + HEARTBEAT_MS),
        "the admission's heartbeat outlived the fire's updates",
    )?;
    adapters
        .wake
        .advance_to(slot.base + DUE + HEARTBEAT_MS)
        .await?;
    expect_equal(
        &slot.alarm_at().await?,
        &Some(LATER),
        "the heartbeat's instance armed the committed wake",
    )?;
    adapters.wake.advance_to(slot.base + LATER).await?;
    expect_equal(
        &slot.ran().await?,
        &vec!["a", "b"],
        "each task ran exactly once",
    )?;
    expect_equal(
        &slot.alarm_at().await?,
        &Option::<i64>::None,
        "and the alarm is cleared",
    )
}

pub fn kill_wake_declarations() -> Vec<KillWakeDeclaration> {
    vec![
        KillWakeDeclaration {
            id: "KILL-WAKE-01",
            operation: KillWakeOperation::Admit,
            point: "alarm write before durable",
            placement: "everywhere",
        },
        KillWakeDeclaration {
            id: "KILL-WAKE-02",
            operation: KillWakeOperation::Admit,
            point: "after the arm, before work",
            placement: "everywhere",
        },
        KillWakeDeclaration {
            id: "KILL-WAKE-03",
            operation: KillWakeOperation::Commit,
            point: "after the commit is durable, before the alarm update",
            placement: "everywhere",
        },
        KillWakeDeclaration {
            id: "KILL-WAKE-04",
            operation: KillWakeOperation::Update,
            point: "eviction during the alarm write",
            placement: "everywhere",
        },
        KillWakeDeclaration {
            id: "KILL-WAKE-05",
            operation: KillWakeOperation::Fire,
            point: "after the re-arm, before resume",
            placement: "everywhere",
        },
        KillWakeDeclaration {
            id: "KILL-WAKE-06",
            operation: KillWakeOperation::Fire,
            point: "after a resumed task's commit is durable",
            placement: "everywhere",
        },
        KillWakeDeclaration {
            id: "KILL-WAKE-07",
            operation: KillWakeOperation::Fire,
            point: "eviction during the alarm clear after the work",
            placement: "everywhere",
        },
        KillWakeDeclaration {
            id: "KILL-WAKE-08",
            operation: KillWakeOperation::Admit,
            point: "a concurrent fire's updates while the admission is uncommitted, then a kill after its commit",
            placement: "everywhere",
        },
    ]
}

pub const KILL_WAKE_DECLARATIONS: fn() -> Vec<KillWakeDeclaration> = kill_wake_declarations;

pub fn kill_wake_cases() -> BTreeMap<&'static str, CaseImplementation<WakeCaseAdapters>> {
    let mut map: BTreeMap<&'static str, CaseImplementation<WakeCaseAdapters>> = BTreeMap::new();
    map.insert("KILL-WAKE-01", Arc::new(|a| Box::pin(kill_wake_01(a))));
    map.insert("KILL-WAKE-02", Arc::new(|a| Box::pin(kill_wake_02(a))));
    map.insert("KILL-WAKE-03", Arc::new(|a| Box::pin(kill_wake_03(a))));
    map.insert("KILL-WAKE-04", Arc::new(|a| Box::pin(kill_wake_04(a))));
    map.insert("KILL-WAKE-05", Arc::new(|a| Box::pin(kill_wake_05(a))));
    map.insert("KILL-WAKE-06", Arc::new(|a| Box::pin(kill_wake_06(a))));
    map.insert("KILL-WAKE-07", Arc::new(|a| Box::pin(kill_wake_07(a))));
    map.insert("KILL-WAKE-08", Arc::new(|a| Box::pin(kill_wake_08(a))));
    map
}

pub const KILL_WAKE_CASES: fn() -> BTreeMap<&'static str, CaseImplementation<WakeCaseAdapters>> =
    kill_wake_cases;
