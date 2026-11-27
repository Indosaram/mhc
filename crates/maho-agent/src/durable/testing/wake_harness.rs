//! Port of senpi `packages/agent/src/durable/testing/wake-harness.ts`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;

use crate::durable::store::{
    DurableStore, DurableStoreError, DurableStoreOptions, RecordScan, StoreOperation,
    StoreWritePoint,
};
use crate::durable::wake_host::{
    OnWakePoint, WakeAlarm, WakeAlarmOptions, WakeAlarmPoint, WakeAlarmStage, WakeHost,
    WakeHostOptions, WakeOp, WakePoint, create_wake_host,
};

use super::store_harness::{StoreConformanceAdapter, is_cut_signal};
use super::wake_test_loop::{TestLoop, open_test_loop};

pub const HEARTBEAT_MS: i64 = 60_000;

/// The session a host's alarm belongs to.
pub struct WakeAlarmSession {
    pub medium: String,
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub on_fire: Arc<dyn Fn() -> BoxFuture<'static, Result<(), DurableStoreError>> + Send + Sync>,
    pub options: WakeAlarmOptions,
}

/// What a host provides to the WAKE, KILL-WAKE and MULTI-HOST cases, next to its store adapter.
pub trait WakeConformanceAdapter: Send + Sync {
    /// The host clock, epoch milliseconds; only `advance_to` moves it.
    fn now(&self) -> i64;
    /// Move the clock to `at`, delivering every alarm that falls due on the way, in time order, each awaited.
    fn advance_to(&self, at: i64) -> BoxFuture<'_, Result<(), String>>;
    /// The durable alarm of one session.
    fn open_alarm(
        &self,
        session: WakeAlarmSession,
    ) -> BoxFuture<'_, Result<Arc<dyn WakeAlarm>, DurableStoreError>>;
    /// The host process restarts: instances and in-memory timers are lost, durable alarms are kept.
    fn restart(&self) -> BoxFuture<'_, Result<(), String>>;
}

#[derive(Clone)]
pub struct WakeCaseAdapters {
    pub store: Arc<dyn StoreConformanceAdapter>,
    pub wake: Arc<dyn WakeConformanceAdapter>,
}

fn cut_nth<P, F>(
    matches: F,
    nth: usize,
) -> Arc<dyn Fn(P) -> Result<(), DurableStoreError> + Send + Sync>
where
    P: std::fmt::Debug + 'static,
    F: Fn(&P) -> bool + Send + Sync + 'static,
{
    let seen = Arc::new(AtomicUsize::new(0));
    Arc::new(move |point| {
        if matches(&point) && seen.fetch_add(1, Ordering::SeqCst) + 1 == nth {
            return Err(DurableStoreError::Cut(format!("cut at {point:?}")));
        }
        Ok(())
    })
}

pub fn cut_store(op: StoreOperation, at: &'static str, nth: usize) -> DurableStoreOptions {
    DurableStoreOptions {
        on_write_point: Some(cut_nth(
            move |point: &StoreWritePoint| match (point, at) {
                (StoreWritePoint::Before { op: p }, "before") => *p == op,
                (StoreWritePoint::Durable { op: p }, "durable") => *p == op,
                _ => false,
            },
            nth,
        )),
    }
}

pub fn cut_alarm(op: &'static str, at: WakeAlarmStage, nth: usize) -> WakeAlarmOptions {
    WakeAlarmOptions {
        on_alarm_point: Some(cut_nth(
            move |point: &WakeAlarmPoint| match (point, op) {
                (WakeAlarmPoint::Set { at: stage, .. }, "set") => *stage == at,
                (WakeAlarmPoint::Clear { at: stage }, "clear") => *stage == at,
                _ => false,
            },
            nth,
        )),
    }
}

pub fn cut_wake(
    op: WakeOp,
    nth: usize,
) -> Arc<dyn Fn(WakePoint) -> Result<(), DurableStoreError> + Send + Sync> {
    cut_nth(move |point: &WakePoint| point.op == op, nth)
}

pub async fn expect_wake_cut<T>(
    result: Result<T, DurableStoreError>,
    what: &str,
) -> Result<(), String> {
    match result {
        Err(ref err) if is_cut_signal(err) => Ok(()),
        Err(err) => Err(format!("{what}: expected the cut to surface, got {err}")),
        Ok(_) => Err(format!(
            "{what}: expected the cut to surface, but it resolved"
        )),
    }
}

#[derive(Clone)]
pub struct SessionInstance {
    pub loop_handle: TestLoop,
    pub host: WakeHost,
}

#[derive(Clone, Default)]
pub struct BootOptions {
    pub store: DurableStoreOptions,
    pub alarm: WakeAlarmOptions,
    pub on_wake_point: Option<OnWakePoint>,
}

#[derive(Clone)]
pub struct SessionSlot {
    pub base: i64,
    inner: Arc<SlotInner>,
}

struct SlotInner {
    adapters: WakeCaseAdapters,
    medium: String,
    reader: Arc<dyn DurableStore>,
    epoch: u64,
    base: i64,
    trace: Mutex<Vec<String>>,
    live: Mutex<Option<SessionInstance>>,
    alarm: Mutex<Option<Arc<dyn WakeAlarm>>>,
    fires: AtomicUsize,
}

impl SlotInner {
    fn boot<'a>(
        self: &'a Arc<Self>,
        options: BootOptions,
    ) -> BoxFuture<'a, Result<SessionInstance, DurableStoreError>> {
        Box::pin(async move {
            let store = self
                .adapters
                .store
                .open(&self.medium, options.store)
                .await?;
            let host_slot: Arc<Mutex<Option<WakeHost>>> = Arc::new(Mutex::new(None));
            let loop_cell: Arc<Mutex<Option<TestLoop>>> = Arc::new(Mutex::new(None));

            let host_for_commit = Arc::clone(&host_slot);
            let loop_for_commit = Arc::clone(&loop_cell);
            let wake_adapter = Arc::clone(&self.adapters.wake);
            let now_fn: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(move || wake_adapter.now());

            let loop_handle = open_test_loop(
                store,
                self.epoch,
                Arc::clone(&now_fn),
                Arc::new(move || {
                    let host = host_for_commit
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone();
                    let loop_opt = loop_for_commit
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone();
                    Box::pin(async move {
                        if let (Some(h), Some(l)) = (host, loop_opt) {
                            h.update(l.wake()).await?;
                        }
                        Ok(())
                    })
                }),
            )
            .await?;
            *loop_cell.lock().unwrap_or_else(|p| p.into_inner()) = Some(loop_handle.clone());

            let base = self.base;
            let inner_alarm = Arc::clone(self);
            let inner_hook = options.alarm.on_alarm_point;
            let alarm_options = WakeAlarmOptions {
                on_alarm_point: Some(Arc::new(move |point| {
                    if let Some(hook) = &inner_hook {
                        hook(point)?;
                    }
                    match point {
                        WakeAlarmPoint::Set {
                            at: WakeAlarmStage::Durable,
                            time,
                        } => {
                            inner_alarm
                                .trace
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push(format!("set:+{}", time - base));
                        }
                        WakeAlarmPoint::Clear {
                            at: WakeAlarmStage::Durable,
                        } => {
                            inner_alarm
                                .trace
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .push("clear".to_owned());
                        }
                        _ => {}
                    }
                    Ok(())
                })),
            };

            let alarm_store = self
                .adapters
                .store
                .open(&self.medium, DurableStoreOptions::default())
                .await?;
            let self_for_fire = Arc::clone(self);
            let alarm = self
                .adapters
                .wake
                .open_alarm(WakeAlarmSession {
                    medium: self.medium.clone(),
                    store: alarm_store,
                    epoch: self.epoch,
                    on_fire: Arc::new(move || {
                        let s = Arc::clone(&self_for_fire);
                        Box::pin(async move { s.dispatch().await })
                    }),
                    options: alarm_options,
                })
                .await?;
            *self.alarm.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&alarm));

            let loop_for_wake = loop_handle.clone();
            let loop_for_resume = loop_handle.clone();
            let self_for_resume = Arc::clone(self);
            let host = create_wake_host(WakeHostOptions {
                alarm,
                now: now_fn,
                wake: Arc::new(move || loop_for_wake.wake()),
                resume: Arc::new(move || {
                    let s = Arc::clone(&self_for_resume);
                    let l = loop_for_resume.clone();
                    Box::pin(async move {
                        s.trace
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .push("resume".to_owned());
                        l.resume().await
                    })
                }),
                heartbeat_ms: Some(HEARTBEAT_MS),
                on_wake_point: options.on_wake_point,
            });
            *host_slot.lock().unwrap_or_else(|p| p.into_inner()) = Some(host.clone());

            let instance = SessionInstance {
                loop_handle,
                host: host.clone(),
            };
            *self.live.lock().unwrap_or_else(|p| p.into_inner()) = Some(instance.clone());
            host.start().await?;
            Ok(instance)
        })
    }

    async fn dispatch(self: &Arc<Self>) -> Result<(), DurableStoreError> {
        self.fires.fetch_add(1, Ordering::SeqCst);
        let existing = self.live.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let outcome = match existing {
            Some(inst) => inst.host.fire().await,
            None => match Box::pin(self.boot(BootOptions::default())).await {
                Ok(inst) => inst.host.fire().await,
                Err(err) => Err(err),
            },
        };
        match outcome {
            Ok(()) => Ok(()),
            Err(err) if is_cut_signal(&err) => {
                *self.live.lock().unwrap_or_else(|p| p.into_inner()) = None;
                Ok(())
            }
            Err(err) => Err(err),
        }
    }
}

impl SessionSlot {
    pub fn trace(&self) -> Vec<String> {
        self.inner
            .trace
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn push_trace(&self, entry: String) {
        self.inner
            .trace
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(entry);
    }

    pub fn clear_trace(&self) {
        self.inner
            .trace
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    pub async fn instance(
        &self,
        options: BootOptions,
    ) -> Result<SessionInstance, DurableStoreError> {
        let existing = self
            .inner
            .live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(inst) = existing {
            return Ok(inst);
        }
        match self.inner.boot(options).await {
            Ok(inst) => Ok(inst),
            Err(err) => {
                if is_cut_signal(&err) {
                    *self.inner.live.lock().unwrap_or_else(|p| p.into_inner()) = None;
                }
                Err(err)
            }
        }
    }

    pub fn kill(&self) {
        *self.inner.live.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    pub fn fires(&self) -> usize {
        self.inner.fires.load(Ordering::SeqCst)
    }

    pub async fn ran(&self) -> Result<Vec<String>, String> {
        let records = self
            .inner
            .reader
            .scan_records(RecordScan {
                kind: Some("ran".to_owned()),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(records
            .into_iter()
            .filter_map(|r| r.id.strip_prefix("ran:").map(str::to_owned))
            .collect())
    }

    pub async fn alarm_at(&self) -> Result<Option<i64>, String> {
        let alarm = self
            .inner
            .alarm
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let Some(alarm) = alarm else {
            return Ok(None);
        };
        let at = alarm.read().await.map_err(|e| e.to_string())?;
        Ok(at.map(|t| t - self.base))
    }
}

pub async fn create_slot(adapters: WakeCaseAdapters, epoch: u64) -> Result<SessionSlot, String> {
    let medium = adapters
        .store
        .create_medium()
        .await
        .map_err(|e| e.to_string())?;
    let reader = adapters
        .store
        .open(&medium, DurableStoreOptions::default())
        .await
        .map_err(|e| e.to_string())?;
    for next in 1..=epoch {
        reader
            .claim(next - 1, next)
            .await
            .map_err(|e| e.to_string())?;
    }
    let base = adapters.wake.now();
    Ok(SessionSlot {
        base,
        inner: Arc::new(SlotInner {
            adapters,
            medium,
            reader,
            epoch,
            base,
            trace: Mutex::new(Vec::new()),
            live: Mutex::new(None),
            alarm: Mutex::new(None),
            fires: AtomicUsize::new(0),
        }),
    })
}

pub async fn restart_host(
    adapters: &WakeCaseAdapters,
    slots: &[SessionSlot],
) -> Result<(), String> {
    adapters.wake.restart().await?;
    for slot in slots {
        slot.kill();
    }
    Ok(())
}
