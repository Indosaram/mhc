//! Port of senpi `packages/agent/src/durable/wake-host.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_ai::types::BoxFuture;

use super::store::DurableStoreError;

/// The loop's wake state: `active` while some work can run now or is running; `at`, the earliest later wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Wake {
    pub active: bool,
    pub at: Option<i64>,
}

/// A host's durable alarm for one session; the armed time (epoch ms) survives instance loss and host restarts.
pub trait WakeAlarm: Send + Sync {
    fn read(&self) -> BoxFuture<'_, Result<Option<i64>, DurableStoreError>>;
    fn set(&self, at: i64) -> BoxFuture<'_, Result<(), DurableStoreError>>;
    fn clear(&self) -> BoxFuture<'_, Result<(), DurableStoreError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeAlarmPoint {
    Set { at: WakeAlarmStage, time: i64 },
    Clear { at: WakeAlarmStage },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeAlarmStage {
    Before,
    Durable,
}

pub type OnAlarmPoint = Arc<dyn Fn(WakeAlarmPoint) -> Result<(), DurableStoreError> + Send + Sync>;

#[derive(Clone, Default)]
pub struct WakeAlarmOptions {
    pub on_alarm_point: Option<OnAlarmPoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeOp {
    Start,
    Admit,
    Fire,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakePoint {
    pub op: WakeOp,
}

pub type OnWakePoint = Arc<dyn Fn(WakePoint) -> Result<(), DurableStoreError> + Send + Sync>;

pub struct WakeHostOptions {
    pub alarm: Arc<dyn WakeAlarm>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub wake: Arc<dyn Fn() -> Wake + Send + Sync>,
    pub resume: Arc<dyn Fn() -> BoxFuture<'static, Result<(), DurableStoreError>> + Send + Sync>,
    pub heartbeat_ms: Option<i64>,
    pub on_wake_point: Option<OnWakePoint>,
}

pub const DEFAULT_HEARTBEAT_MS: i64 = 60_000;

/// The alarm time for a wake state: a heartbeat while active, never later than the earliest wake.
fn recovery_alarm_at(wake: Wake, now: i64, heartbeat_ms: i64) -> Option<i64> {
    let heartbeat = wake.active.then_some(now + heartbeat_ms);
    match (heartbeat, wake.at) {
        (Some(hb), Some(at)) => Some(hb.min(at)),
        (Some(hb), None) => Some(hb),
        (None, Some(at)) => Some(at),
        (None, None) => None,
    }
}

struct AdmittedGuard<'a> {
    counter: &'a AtomicUsize,
}

impl Drop for AdmittedGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub struct WakeHost {
    alarm: Arc<dyn WakeAlarm>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    wake: Arc<dyn Fn() -> Wake + Send + Sync>,
    resume: Arc<dyn Fn() -> BoxFuture<'static, Result<(), DurableStoreError>> + Send + Sync>,
    heartbeat_ms: i64,
    on_wake_point: Option<OnWakePoint>,
    admitted: Arc<AtomicUsize>,
}

impl WakeHost {
    fn point(&self, op: WakeOp) -> Result<(), DurableStoreError> {
        if let Some(hook) = &self.on_wake_point {
            hook(WakePoint { op })?;
        }
        Ok(())
    }

    async fn arm(&self, state: Wake) -> Result<(), DurableStoreError> {
        let effective = if self.admitted.load(Ordering::SeqCst) > 0 {
            Wake {
                active: true,
                at: state.at,
            }
        } else {
            state
        };
        let at = recovery_alarm_at(effective, (self.now)(), self.heartbeat_ms);
        match at {
            None => self.alarm.clear().await,
            Some(at) => self.alarm.set(at).await,
        }
    }

    /// A new instance: arm a fallback, resume, then arm from the loop's wake.
    pub async fn start(&self) -> Result<(), DurableStoreError> {
        let fallback = (self.now)() + self.heartbeat_ms;
        let current = self.alarm.read().await?;
        if current.is_none_or(|time| time > fallback) {
            self.alarm.set(fallback).await?;
        }
        self.point(WakeOp::Start)?;
        (self.resume)().await?;
        self.arm((self.wake)()).await
    }

    /// Run work that can commit, after a recovery heartbeat is durable.
    pub async fn admit<T, F, Fut>(&self, work: F) -> Result<T, DurableStoreError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, DurableStoreError>>,
    {
        self.admitted.fetch_add(1, Ordering::SeqCst);
        let result = {
            let _guard = AdmittedGuard {
                counter: &self.admitted,
            };
            let current = (self.wake)();
            self.arm(Wake {
                active: true,
                at: current.at,
            })
            .await?;
            self.point(WakeOp::Admit)?;
            work().await?
        };
        if self.admitted.load(Ordering::SeqCst) == 0 {
            self.arm((self.wake)()).await?;
        }
        Ok(result)
    }

    pub async fn update(&self, wake: Wake) -> Result<(), DurableStoreError> {
        self.arm(wake).await
    }

    /// The alarm fired: re-arm the heartbeat, resume, then arm from the loop's wake.
    pub async fn fire(&self) -> Result<(), DurableStoreError> {
        let current = (self.wake)();
        self.arm(Wake {
            active: true,
            at: current.at,
        })
        .await?;
        self.point(WakeOp::Fire)?;
        (self.resume)().await?;
        self.arm((self.wake)()).await
    }
}

pub fn create_wake_host(options: WakeHostOptions) -> WakeHost {
    WakeHost {
        alarm: options.alarm,
        now: options.now,
        wake: options.wake,
        resume: options.resume,
        heartbeat_ms: options.heartbeat_ms.unwrap_or(DEFAULT_HEARTBEAT_MS),
        on_wake_point: options.on_wake_point,
        admitted: Arc::new(AtomicUsize::new(0)),
    }
}
