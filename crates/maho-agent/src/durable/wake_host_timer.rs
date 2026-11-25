//! Port of senpi `packages/agent/src/durable/wake-host-bun-timer.ts`, adapted to a Tokio timer abstraction.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use maho_ai::types::BoxFuture;

use super::store::DurableStoreError;
use super::wake_host::{WakeAlarm, WakeAlarmOptions, WakeAlarmPoint, WakeAlarmStage};
use super::wake_host_store_alarm::AlarmPersistence;

pub type TimerCancel = Arc<dyn Fn() + Send + Sync>;
pub type TimerCallback = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send + 'static>;
pub type SetTimer = Arc<dyn Fn(TimerCallback, u64) -> TimerCancel + Send + Sync>;

pub type OnFireError = Arc<dyn Fn(&str, &DurableStoreError) + Send + Sync>;

pub struct TokioTimerHostOptions {
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub set_timer: Option<SetTimer>,
    /// Longest single timer; default 60 s.
    pub max_delay_ms: Option<i64>,
    pub on_fire_error: OnFireError,
}

pub type BunTimerHostOptions = TokioTimerHostOptions;
pub type BunTimerHost = TokioTimerHost;

const DEFAULT_MAX_DELAY_MS: i64 = 60_000;

fn default_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn default_set_timer() -> SetTimer {
    Arc::new(|callback: TimerCallback, ms: u64| {
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            callback().await;
        });
        Arc::new(move || handle.abort())
    })
}

struct Entry {
    id: u64,
    at: Option<i64>,
    cancel: Option<TimerCancel>,
    on_fire: Arc<dyn Fn() -> BoxFuture<'static, Result<(), DurableStoreError>> + Send + Sync>,
}

struct Inner {
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    set_timer: SetTimer,
    max_delay_ms: i64,
    on_fire_error: OnFireError,
    state: Mutex<HostState>,
}

#[derive(Default)]
struct HostState {
    next_id: u64,
    entries: HashMap<String, Arc<Mutex<Entry>>>,
    delivering: HashSet<String>,
}

impl Inner {
    fn schedule(self: &Arc<Self>, key: &str, entry_arc: &Arc<Mutex<Entry>>, retry: bool) {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(current) = state.entries.get(key) else {
            return;
        };
        if !Arc::ptr_eq(current, entry_arc) {
            return;
        }
        let mut entry = entry_arc.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cancel) = entry.cancel.take() {
            cancel();
        }
        let Some(at) = entry.at else {
            return;
        };
        if state.delivering.contains(key) {
            return;
        }
        let delay = if retry {
            self.max_delay_ms
        } else {
            (at - (self.now)()).max(0).min(self.max_delay_ms)
        };
        let host = Arc::clone(self);
        let key_owned = key.to_owned();
        let entry_weak = Arc::clone(entry_arc);
        let cancel = (self.set_timer)(
            Box::new(move || {
                Box::pin(async move {
                    host.tick(&key_owned, &entry_weak).await;
                })
            }),
            delay.max(0) as u64,
        );
        entry.cancel = Some(cancel);
    }

    async fn tick(self: &Arc<Self>, key: &str, entry_arc: &Arc<Mutex<Entry>>) {
        let (fired_at, on_fire) = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(current) = state.entries.get(key) else {
                return;
            };
            if !Arc::ptr_eq(current, entry_arc) {
                return;
            }
            let mut entry = entry_arc.lock().unwrap_or_else(|p| p.into_inner());
            entry.cancel = None;
            let Some(fired_at) = entry.at else {
                return;
            };
            if (self.now)() < fired_at {
                drop(entry);
                drop(state);
                self.schedule(key, entry_arc, false);
                return;
            }
            state.delivering.insert(key.to_owned());
            (fired_at, Arc::clone(&entry.on_fire))
        };

        let result = on_fire().await;
        if let Err(error) = &result {
            (self.on_fire_error)(key, error);
        }

        let current_arc = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.delivering.remove(key);
            state.entries.get(key).cloned()
        };
        if let Some(current_arc) = current_arc {
            let retry = {
                let current = current_arc.lock().unwrap_or_else(|p| p.into_inner());
                Arc::ptr_eq(&current_arc, entry_arc) && current.at == Some(fired_at)
            };
            self.schedule(key, &current_arc, retry);
        }
    }
}

struct TimerWakeAlarm {
    host: Arc<Inner>,
    key: String,
    entry_id: u64,
    entry: Arc<Mutex<Entry>>,
    persistence: Arc<dyn AlarmPersistence>,
    options: WakeAlarmOptions,
}

impl TimerWakeAlarm {
    async fn write(&self, at: Option<i64>) -> Result<(), DurableStoreError> {
        {
            let state = self.host.state.lock().unwrap_or_else(|p| p.into_inner());
            let active = state
                .entries
                .get(&self.key)
                .is_some_and(|e| e.lock().unwrap_or_else(|p| p.into_inner()).id == self.entry_id);
            if !active {
                return Err(DurableStoreError::Other(format!(
                    "the alarm instance of {} was replaced by a newer one",
                    self.key
                )));
            }
        }
        let point = |stage: WakeAlarmStage| {
            if let Some(hook) = &self.options.on_alarm_point {
                let pt = match at {
                    None => WakeAlarmPoint::Clear { at: stage },
                    Some(time) => WakeAlarmPoint::Set { at: stage, time },
                };
                hook(pt)?;
            }
            Ok::<(), DurableStoreError>(())
        };
        point(WakeAlarmStage::Before)?;
        self.persistence.write(at).await?;
        {
            let mut entry = self.entry.lock().unwrap_or_else(|p| p.into_inner());
            entry.at = at;
        }
        self.host.schedule(&self.key, &self.entry, false);
        point(WakeAlarmStage::Durable)?;
        Ok(())
    }
}

impl WakeAlarm for TimerWakeAlarm {
    fn read(&self) -> BoxFuture<'_, Result<Option<i64>, DurableStoreError>> {
        self.persistence.read()
    }

    fn set(&self, at: i64) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        Box::pin(async move { self.write(Some(at)).await })
    }

    fn clear(&self) -> BoxFuture<'_, Result<(), DurableStoreError>> {
        Box::pin(async move { self.write(None).await })
    }
}

/// Process timer host (Tokio-backed): one instance serves every session under its own key.
#[derive(Clone)]
pub struct TokioTimerHost {
    inner: Arc<Inner>,
}

impl TokioTimerHost {
    pub async fn open_alarm(
        &self,
        key: impl Into<String>,
        persistence: Arc<dyn AlarmPersistence>,
        on_fire: Arc<dyn Fn() -> BoxFuture<'static, Result<(), DurableStoreError>> + Send + Sync>,
        options: WakeAlarmOptions,
    ) -> Result<Arc<dyn WakeAlarm>, DurableStoreError> {
        let key = key.into();
        let initial_at = persistence.read().await?;
        let entry_arc = {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            state.next_id += 1;
            let id = state.next_id;
            let entry = Arc::new(Mutex::new(Entry {
                id,
                at: initial_at,
                cancel: None,
                on_fire,
            }));
            if let Some(prev) = state.entries.insert(key.clone(), Arc::clone(&entry))
                && let Some(cancel) = prev.lock().unwrap_or_else(|p| p.into_inner()).cancel.take()
            {
                cancel();
            }
            entry
        };
        let entry_id = entry_arc.lock().unwrap_or_else(|p| p.into_inner()).id;
        self.inner.schedule(&key, &entry_arc, false);
        Ok(Arc::new(TimerWakeAlarm {
            host: Arc::clone(&self.inner),
            key,
            entry_id,
            entry: entry_arc,
            persistence,
            options,
        }))
    }

    /// Tick manually if invoked from a synchronous timer callback in tests.
    pub async fn fire_entry(&self, key: &str, entry_id: u64) {
        let found = {
            let state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            state.entries.get(key).cloned()
        };
        if let Some(entry_arc) = found {
            let matches = entry_arc.lock().unwrap_or_else(|p| p.into_inner()).id == entry_id;
            if matches {
                self.inner.tick(key, &entry_arc).await;
            }
        }
    }

    pub fn close(&self) {
        let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
        for (_, entry_arc) in state.entries.drain() {
            if let Some(cancel) = entry_arc
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .cancel
                .take()
            {
                cancel();
            }
        }
    }
}

pub fn create_tokio_timer_host(options: TokioTimerHostOptions) -> TokioTimerHost {
    TokioTimerHost {
        inner: Arc::new(Inner {
            now: options.now.unwrap_or_else(|| Arc::new(default_now_ms)),
            set_timer: options.set_timer.unwrap_or_else(default_set_timer),
            max_delay_ms: options.max_delay_ms.unwrap_or(DEFAULT_MAX_DELAY_MS),
            on_fire_error: options.on_fire_error,
            state: Mutex::new(HostState::default()),
        }),
    }
}

pub fn create_bun_timer_host(options: BunTimerHostOptions) -> BunTimerHost {
    create_tokio_timer_host(options)
}
