//! Woken session carrier & lease manager (`thread/wake/carrier.ts`).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tokio::sync::Mutex as AsyncMutex;

use super::super::constants::WAKE_LEASE_MS;
use super::super::intake_admission::IntakeAdmission;
use super::super::session_wait_types::{
    SessionWaitKind, WriteSessionWaitRequest, wait_blocks_delivery,
};
use super::super::store_api::GatewayStoreApi;
use super::super::woken_types::{
    WakeRetryOutcome, WakeStartOutcome, WakeTakeOverOutcome, WokenSessionState, WokenStateUpdate,
};
use super::carrier_connection::CarrierHostConnection;
use super::closed_intake::settle_closed_intake;
use super::engine_state::{EngineView, read_engine_state};
use super::memory_headroom::WAKE_MEMORY_FLOOR_BYTES;
use super::session_target::{WakeSessionKind, WakeTarget, resolve_wake_target};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

const PERSON_WAIT_CLOSE_MS: i64 = 30 * 60_000;
const CARRIER_RENEW_MS: i64 = 20_000;
const QUEUED_DELIVERY_GRACE_MS: i64 = 2 * 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarrierWakeOutcome {
    Open {
        session_id: String,
        generation: i64,
    },
    Held,
    Waiting,
    WakeLimitReached,
    Gone,
    Ambiguous {
        session_paths: Vec<String>,
    },
    NotWakeable {
        session_kind: WakeSessionKind,
    },
    HostMemoryPressure {
        available_bytes: u64,
        floor_bytes: u64,
    },
    NotYet {
        next_attempt_at: i64,
    },
    Busy {
        code: String,
    },
    Deferred {
        attempts: i64,
    },
    Failed {
        attempts: i64,
    },
    SwitchedOff,
    NothingQueued,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarrierEvaluation {
    Released,
    KeptWorking,
    HandedOver,
    CloseFailed { code: String },
    Waiting { since: i64 },
    ClosedWaiting,
    Held,
    LostLease,
    NotCarried,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarrierEvent {
    pub durable_id: String,
    pub outcome: CarrierEvaluation,
}

pub type CarrierConnectFn =
    Arc<dyn Fn() -> BoxFuture<Result<Arc<dyn CarrierHostConnection>, String>> + Send + Sync>;
pub type CarrierSameDeviceFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub struct CarrierOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub sessions_directory: PathBuf,
    pub ensure_host: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>,
    pub connect: CarrierConnectFn,
    pub wake_limit: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    pub same_device: Option<CarrierSameDeviceFn>,
    pub remote_wake: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub admit_intake: Option<Arc<dyn Fn(String) -> BoxFuture<IntakeAdmission> + Send + Sync>>,
    pub memory_headroom: Option<Arc<dyn Fn() -> Option<u64> + Send + Sync>>,
    pub memory_floor: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    pub on_event: Option<Arc<dyn Fn(CarrierEvent) + Send + Sync>>,
}

struct Carried {
    connection: Arc<dyn CarrierHostConnection>,
    session_id: String,
    generation: i64,
    delivery_refused_since: Option<i64>,
}

enum OpenResult {
    Opened {
        connection: Arc<dyn CarrierHostConnection>,
        session_id: String,
        attached: bool,
        view: EngineView,
    },
    Refused {
        code: String,
    },
    Unreachable,
}

enum QuietRelease {
    Ok,
    Err { code: String, for_delivery: bool },
}

enum Allowance {
    Open,
    SwitchedOff,
    NothingQueued,
}

fn code_of(frame: &Value) -> String {
    if let Some(c) = frame.get("errorCode").and_then(Value::as_str) {
        return c.to_owned();
    }
    if let Some(err) = frame.get("error").and_then(Value::as_str) {
        return err.split(':').next().unwrap_or(err).trim().to_owned();
    }
    "unknown".to_owned()
}

fn data_of(frame: &Value) -> Value {
    frame
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()))
}

fn busy_list(frame: &Value) -> Vec<String> {
    frame
        .get("errorData")
        .and_then(|d| d.get("busy"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn file_size(path: &str) -> i64 {
    std::fs::metadata(Path::new(path))
        .map(|m| m.len() as i64)
        .unwrap_or(-1)
}

fn device_of_ledger(ledger_id: &str) -> Option<&str> {
    let rest = ledger_id.strip_prefix("device:")?;
    let slash = rest.find('/')?;
    (slash > 0).then_some(&rest[..slash])
}

fn is_held_elsewhere(code: &str) -> bool {
    matches!(code, "session_held" | "session_path_in_use")
}

fn is_definitive(code: &str) -> bool {
    matches!(code, "invalid_path" | "unknown_session")
}

fn json_obj(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    let mut map = Map::new();
    for (k, v) in pairs {
        map.insert(k.to_owned(), v);
    }
    map
}

pub struct Carrier {
    options: CarrierOptions,
    carried: Mutex<HashMap<String, Carried>>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    stopped: std::sync::atomic::AtomicBool,
    timer_armed: std::sync::atomic::AtomicBool,
    sweep_wake: Arc<tokio::sync::Notify>,
}

impl Carrier {
    pub fn new(options: CarrierOptions) -> Arc<Self> {
        let carrier = Arc::new(Self {
            options,
            carried: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
            stopped: std::sync::atomic::AtomicBool::new(false),
            timer_armed: std::sync::atomic::AtomicBool::new(false),
            sweep_wake: Arc::new(tokio::sync::Notify::new()),
        });
        let c = carrier.clone();
        if let Ok(rows) = c.options.store.woken_list()
            && !rows.is_empty()
        {
            tokio::spawn(async move {
                if c.sweep_now().await {
                    c.arm();
                }
            });
        }
        carrier
    }

    fn lock_for(&self, durable_id: &str) -> Arc<AsyncMutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(durable_id.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn emit(&self, durable_id: &str, outcome: &CarrierEvaluation) {
        if let Some(ref cb) = self.options.on_event {
            cb(CarrierEvent {
                durable_id: durable_id.to_owned(),
                outcome: outcome.clone(),
            });
        }
    }

    fn forget(&self, durable_id: &str) {
        if let Some(entry) = self
            .carried
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(durable_id)
        {
            entry.connection.close();
        }
    }

    async fn release_own(&self, conn: &Arc<dyn CarrierHostConnection>, session_id: &str) {
        let _ = conn
            .request(json_obj(vec![
                ("type", Value::String("release_attachment".into())),
                ("sessionId", Value::String(session_id.to_owned())),
            ]))
            .await;
    }

    fn record_held(&self, durable_id: &str, session_file: &str, view: &EngineView) {
        let kinds = if view.hold_kinds.is_empty() {
            vec!["paused".to_owned()]
        } else {
            view.hold_kinds.clone()
        };
        let _ = self
            .options
            .store
            .session_wait_write(&WriteSessionWaitRequest {
                durable_id: durable_id.to_owned(),
                wait: SessionWaitKind::Held,
                hold_kinds: kinds,
                file_size: file_size(session_file),
            });
    }

    async fn quiet_release(
        &self,
        conn: &Arc<dyn CarrierHostConnection>,
        session_id: &str,
        drop_held_delivery: bool,
    ) -> QuietRelease {
        let Ok(first) = conn
            .request(json_obj(vec![
                ("type", Value::String("release_session".into())),
                ("sessionId", Value::String(session_id.to_owned())),
                ("reason", Value::String("takeover".into())),
            ]))
            .await
        else {
            return QuietRelease::Err {
                code: "unreachable".into(),
                for_delivery: false,
            };
        };
        if first.get("success").and_then(Value::as_bool) == Some(true) {
            return QuietRelease::Ok;
        }
        let code = code_of(&first);
        let for_delivery =
            code == "turn_active" && busy_list(&first).iter().any(|b| b == "delivery");
        if !drop_held_delivery && for_delivery {
            return QuietRelease::Err {
                code: "turn_active".into(),
                for_delivery: true,
            };
        }
        if for_delivery {
            return match conn
                .request(json_obj(vec![
                    ("type", Value::String("release_session".into())),
                    ("sessionId", Value::String(session_id.to_owned())),
                    ("reason", Value::String("takeover".into())),
                    ("interrupt", Value::Bool(true)),
                ]))
                .await
            {
                Ok(second) if second.get("success").and_then(Value::as_bool) == Some(true) => {
                    QuietRelease::Ok
                }
                Ok(second) => QuietRelease::Err {
                    code: code_of(&second),
                    for_delivery: false,
                },
                Err(_) => QuietRelease::Err {
                    code: "unreachable".into(),
                    for_delivery: false,
                },
            };
        }
        QuietRelease::Err {
            code,
            for_delivery: false,
        }
    }

    async fn attached_others(
        &self,
        conn: &Arc<dyn CarrierHostConnection>,
        session_id: &str,
    ) -> bool {
        let Ok(listed) = conn
            .request(json_obj(vec![
                ("type", Value::String("list_sessions".into())),
                ("observe", Value::Bool(true)),
            ]))
            .await
        else {
            return false;
        };
        let data = data_of(&listed);
        let Some(sessions) = data.get("sessions").and_then(Value::as_array) else {
            return false;
        };
        sessions
            .iter()
            .find(|s| s.get("sessionId").and_then(Value::as_str) == Some(session_id))
            .and_then(|s| s.get("attachments"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 1
    }

    async fn evaluate_now(&self, durable_id: &str) -> CarrierEvaluation {
        let (conn, session_id, generation, delivery_refused_since) = {
            let guard = self
                .carried
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(e) = guard.get(durable_id) else {
                return CarrierEvaluation::NotCarried;
            };
            (
                e.connection.clone(),
                e.session_id.clone(),
                e.generation,
                e.delivery_refused_since,
            )
        };
        let Ok(renewed) = self.options.store.woken_renew(durable_id, generation) else {
            self.release_own(&conn, &session_id).await;
            self.forget(durable_id);
            return CarrierEvaluation::LostLease;
        };
        let Some(renewed_row) = renewed else {
            self.release_own(&conn, &session_id).await;
            self.forget(durable_id);
            return CarrierEvaluation::LostLease;
        };
        if self.attached_others(&conn, &session_id).await {
            self.release_own(&conn, &session_id).await;
            let _ = self.options.store.woken_end(durable_id, generation);
            let _ = self.options.store.woken_reset_count(durable_id);
            self.forget(durable_id);
            return CarrierEvaluation::HandedOver;
        }
        let state_frame = conn
            .request(json_obj(vec![
                ("type", Value::String("get_state".into())),
                ("sessionId", Value::String(session_id.clone())),
            ]))
            .await
            .unwrap_or(Value::Null);
        let view = read_engine_state(&data_of(&state_frame));
        if view.held {
            let _ = self.quiet_release(&conn, &session_id, true).await;
            self.record_held(durable_id, &renewed_row.session_file, &view);
            let _ = self.options.store.woken_end(durable_id, generation);
            self.forget(durable_id);
            return CarrierEvaluation::Held;
        }
        if view.waiting_for_person {
            let recorded = self
                .options
                .store
                .woken_record_waiting(durable_id, generation);
            let Some(row) = (match recorded {
                Ok(r) => r,
                Err(_) => return CarrierEvaluation::LostLease,
            }) else {
                return CarrierEvaluation::LostLease;
            };
            let since = row
                .first_waiting_at
                .unwrap_or_else(|| self.options.store.now());
            if self.options.store.now() - since < PERSON_WAIT_CLOSE_MS {
                return CarrierEvaluation::Waiting { since };
            }
            let fresh_frame = conn
                .request(json_obj(vec![
                    ("type", Value::String("get_state".into())),
                    ("sessionId", Value::String(session_id.clone())),
                ]))
                .await
                .unwrap_or(Value::Null);
            let fresh = read_engine_state(&data_of(&fresh_frame));
            if !fresh.waiting_for_person {
                return CarrierEvaluation::KeptWorking;
            }
            let _ = conn
                .request(json_obj(vec![
                    ("type", Value::String("close_session".into())),
                    ("sessionId", Value::String(session_id.clone())),
                ]))
                .await;
            let _ = self.options.store.woken_end(durable_id, generation);
            self.forget(durable_id);
            return CarrierEvaluation::ClosedWaiting;
        }
        if renewed_row.first_waiting_at.is_some() {
            let _ = self
                .options
                .store
                .woken_clear_waiting(durable_id, generation);
        }
        if !view.finished {
            if let Some(e) = self
                .carried
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(durable_id)
            {
                e.delivery_refused_since = None;
            }
            return CarrierEvaluation::KeptWorking;
        }
        let now = self.options.store.now();
        let grace_over =
            delivery_refused_since.is_some_and(|since| now - since >= QUEUED_DELIVERY_GRACE_MS);
        match self.quiet_release(&conn, &session_id, grace_over).await {
            QuietRelease::Ok => {
                let _ = conn
                    .request(json_obj(vec![(
                        "type",
                        Value::String("detach_connection".into()),
                    )]))
                    .await;
                let _ = self.options.store.woken_end(durable_id, generation);
                self.forget(durable_id);
                CarrierEvaluation::Released
            }
            QuietRelease::Err { code, for_delivery } => {
                if for_delivery {
                    if let Some(e) = self
                        .carried
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get_mut(durable_id)
                        && e.delivery_refused_since.is_none()
                    {
                        e.delivery_refused_since = Some(now);
                    }
                } else if let Some(e) = self
                    .carried
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_mut(durable_id)
                {
                    e.delivery_refused_since = None;
                }
                if code == "turn_active" || code == "session_busy" {
                    return CarrierEvaluation::KeptWorking;
                }
                if code == "attached" {
                    self.release_own(&conn, &session_id).await;
                    let _ = self.options.store.woken_end(durable_id, generation);
                    let _ = self.options.store.woken_reset_count(durable_id);
                    self.forget(durable_id);
                    return CarrierEvaluation::HandedOver;
                }
                let _ = self.options.store.woken_end(durable_id, generation);
                self.forget(durable_id);
                CarrierEvaluation::CloseFailed { code }
            }
        }
    }

    pub async fn evaluate(self: &Arc<Self>, durable_id: &str) -> CarrierEvaluation {
        let lock = self.lock_for(durable_id);
        let _guard = lock.lock().await;
        let outcome = self.evaluate_now(durable_id).await;
        self.emit(durable_id, &outcome);
        outcome
    }

    fn carry(
        self: &Arc<Self>,
        durable_id: &str,
        connection: Arc<dyn CarrierHostConnection>,
        session_id: String,
        generation: i64,
    ) {
        let entry = Carried {
            connection: connection.clone(),
            session_id: session_id.clone(),
            generation,
            delivery_refused_since: None,
        };
        self.carried
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(durable_id.to_owned(), entry);

        let mut events = connection.subscribe_events();
        let this = self.clone();
        let did = durable_id.to_owned();
        let sid = session_id;
        tokio::spawn(async move {
            let closed = connection.closed_notify();
            loop {
                tokio::select! {
                    _ = closed.notified() => {
                        let mut guard = this
                            .carried
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if guard.get(&did).is_some_and(|e| e.generation == generation) {
                            guard.remove(&did);
                        }
                        break;
                    }
                    maybe = events.recv() => {
                        let Some(frame) = maybe else { break; };
                        if frame.get("type").and_then(Value::as_str) == Some("session_lifecycle")
                            && frame.get("sessionId").and_then(Value::as_str) == Some(sid.as_str())
                        {
                            let _ = this.evaluate(&did).await;
                        }
                    }
                }
            }
        });
    }

    async fn open(&self, session_path: &str, cwd: &str) -> OpenResult {
        (self.options.ensure_host)().await;
        let Ok(connection) = (self.options.connect)().await else {
            return OpenResult::Unreachable;
        };
        let Ok(reply) = connection
            .request(json_obj(vec![
                ("type", Value::String("open_session".into())),
                ("sessionPath", Value::String(session_path.to_owned())),
                ("cwd", Value::String(cwd.to_owned())),
                ("retain_on_disconnect", Value::Bool(true)),
            ]))
            .await
        else {
            connection.close();
            return OpenResult::Unreachable;
        };
        if reply.get("success").and_then(Value::as_bool) != Some(true) {
            let code = code_of(&reply);
            connection.close();
            return OpenResult::Refused { code };
        }
        let data = data_of(&reply);
        let session_id = data
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let attached = data.get("attached").and_then(Value::as_bool) == Some(true);
        let view = read_engine_state(data.get("state").unwrap_or(&Value::Null));
        OpenResult::Opened {
            connection,
            session_id,
            attached,
            view,
        }
    }

    async fn open_allowance(
        &self,
        durable_id: &str,
        cwd: &str,
        closed_at: Option<i64>,
    ) -> Allowance {
        let same_dev = self
            .options
            .same_device
            .as_ref()
            .map(|f| f(cwd))
            .unwrap_or(false);
        let remote_wake = self
            .options
            .remote_wake
            .as_ref()
            .map(|f| f())
            .unwrap_or(false);
        let mut queued = false;
        let Ok(rows) = settle_closed_intake(&*self.options.store, durable_id, closed_at).await
        else {
            return Allowance::NothingQueued;
        };
        for row in rows {
            queued = true;
            let Some(ref ledger_id) = row.ledger_id else {
                if same_dev {
                    return Allowance::Open;
                }
                continue;
            };
            if !remote_wake {
                continue;
            }
            let Some(dev) = device_of_ledger(ledger_id) else {
                continue;
            };
            let Some(ref admit) = self.options.admit_intake else {
                return Allowance::Open;
            };
            if admit(dev.to_owned()).await == IntakeAdmission::Admitted {
                return Allowance::Open;
            }
        }
        if queued {
            Allowance::SwitchedOff
        } else {
            Allowance::NothingQueued
        }
    }

    async fn wake_now(self: &Arc<Self>, durable_id: &str) -> CarrierWakeOutcome {
        if let Some(existing) = self
            .carried
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(durable_id)
        {
            return CarrierWakeOutcome::Open {
                session_id: existing.session_id.clone(),
                generation: existing.generation,
            };
        }
        let target = resolve_wake_target(&self.options.sessions_directory, durable_id);
        let (session_path, cwd, closed_at) = match target {
            WakeTarget::Gone => return CarrierWakeOutcome::Gone,
            WakeTarget::Ambiguous { session_paths } => {
                return CarrierWakeOutcome::Ambiguous { session_paths };
            }
            WakeTarget::Found {
                session_path,
                cwd,
                session_kind,
                closed_at,
            } => {
                if session_kind != WakeSessionKind::Interactive {
                    return CarrierWakeOutcome::NotWakeable { session_kind };
                }
                (session_path, cwd, closed_at)
            }
        };

        if let Ok(Some(wait)) = self.options.store.session_wait_read(durable_id)
            && wait.wait == SessionWaitKind::Held
            && wait_blocks_delivery(&wait)
            && wait.file_size == file_size(&session_path)
        {
            return CarrierWakeOutcome::Held;
        }

        match self.open_allowance(durable_id, &cwd, closed_at).await {
            Allowance::Open => {}
            Allowance::NothingQueued => {
                let rows = self.options.store.woken_list().unwrap_or_default();
                let Some(record) = rows.into_iter().find(|r| r.durable_id == durable_id) else {
                    return CarrierWakeOutcome::NothingQueued;
                };
                if record.state != WokenSessionState::Requested {
                    return CarrierWakeOutcome::Busy {
                        code: "wake_in_progress".into(),
                    };
                }
                let ended = self.options.store.woken_end(durable_id, record.generation);
                return if matches!(ended, Ok(true)) {
                    CarrierWakeOutcome::NothingQueued
                } else {
                    CarrierWakeOutcome::Busy {
                        code: "wake_in_progress".into(),
                    }
                };
            }
            Allowance::SwitchedOff => {
                let _ = self.options.store.woken_request(durable_id, &session_path);
                return CarrierWakeOutcome::SwitchedOff;
            }
        }

        let limit = self.options.wake_limit.as_ref().map(|f| f()).unwrap_or(0);
        if limit > 0 && self.options.store.woken_wake_count(durable_id).unwrap_or(0) as u64 >= limit
        {
            return CarrierWakeOutcome::WakeLimitReached;
        }

        let _ = self.options.store.woken_request(durable_id, &session_path);
        if let Some(avail) = self.options.memory_headroom.as_ref().and_then(|f| f()) {
            let floor = self
                .options
                .memory_floor
                .as_ref()
                .map(|f| f())
                .unwrap_or(WAKE_MEMORY_FLOOR_BYTES);
            if avail < floor {
                return CarrierWakeOutcome::HostMemoryPressure {
                    available_bytes: avail,
                    floor_bytes: floor,
                };
            }
        }

        let started = match self.options.store.woken_start(durable_id) {
            Ok(s) => s,
            Err(_) => {
                return CarrierWakeOutcome::Busy {
                    code: "wake_in_progress".into(),
                };
            }
        };
        let row = match started {
            WakeStartOutcome::NotYet { next_attempt_at } => {
                return CarrierWakeOutcome::NotYet { next_attempt_at };
            }
            WakeStartOutcome::Started { row } => row,
            _ => {
                return CarrierWakeOutcome::Busy {
                    code: "wake_in_progress".into(),
                };
            }
        };

        let opened = self.open(&session_path, &cwd).await;
        let (connection, session_id, attached, view) = match opened {
            OpenResult::Opened {
                connection,
                session_id,
                attached,
                view,
            } => (connection, session_id, attached, view),
            OpenResult::Refused { code } if is_held_elsewhere(&code) || is_definitive(&code) => {
                let _ = self.options.store.woken_end(durable_id, row.generation);
                return if is_definitive(&code) {
                    CarrierWakeOutcome::Gone
                } else {
                    CarrierWakeOutcome::Busy { code }
                };
            }
            OpenResult::Refused { .. } | OpenResult::Unreachable => {
                return match self
                    .options
                    .store
                    .woken_retry_later(durable_id, row.generation)
                {
                    Ok(WakeRetryOutcome::Failed { attempts }) => {
                        CarrierWakeOutcome::Failed { attempts }
                    }
                    Ok(WakeRetryOutcome::LostLease) | Err(_) => CarrierWakeOutcome::Busy {
                        code: "wake_in_progress".into(),
                    },
                    Ok(WakeRetryOutcome::Deferred { row }) => CarrierWakeOutcome::Deferred {
                        attempts: row.attempts,
                    },
                };
            }
        };

        if view.held {
            if attached {
                self.release_own(&connection, &session_id).await;
            } else {
                let _ = self.quiet_release(&connection, &session_id, true).await;
            }
            self.record_held(durable_id, &session_path, &view);
            let _ = self.options.store.woken_end(durable_id, row.generation);
            connection.close();
            return CarrierWakeOutcome::Held;
        }
        if view.waiting_for_person {
            let _ = self
                .options
                .store
                .session_wait_write(&WriteSessionWaitRequest {
                    durable_id: durable_id.to_owned(),
                    wait: SessionWaitKind::Waiting,
                    hold_kinds: Vec::new(),
                    file_size: file_size(&session_path),
                });
            self.release_own(&connection, &session_id).await;
            let _ = self.options.store.woken_end(durable_id, row.generation);
            connection.close();
            return CarrierWakeOutcome::Waiting;
        }

        let _ = self.options.store.session_wait_clear(durable_id);
        let _ = self
            .options
            .store
            .woken_count_wake(durable_id, view.human_input_revision);
        match self.options.store.woken_set_state(&WokenStateUpdate {
            durable_id: durable_id.to_owned(),
            generation: row.generation,
            state: WokenSessionState::Open,
            host_instance: None,
        }) {
            Ok(Some(_)) => {}
            _ => {
                self.release_own(&connection, &session_id).await;
                connection.close();
                return CarrierWakeOutcome::Busy {
                    code: "wake_in_progress".into(),
                };
            }
        }
        self.carry(
            durable_id,
            connection.clone(),
            session_id.clone(),
            row.generation,
        );
        let _ = connection
            .request(json_obj(vec![
                ("type", Value::String("wake".into())),
                ("sessionId", Value::String(session_id.clone())),
            ]))
            .await;
        CarrierWakeOutcome::Open {
            session_id,
            generation: row.generation,
        }
    }

    async fn take_over(self: &Arc<Self>, durable_id: &str) {
        let Ok(WakeTakeOverOutcome::TakenOver { row, .. }) =
            self.options.store.woken_take_over(durable_id)
        else {
            return;
        };
        let target = resolve_wake_target(&self.options.sessions_directory, durable_id);
        let WakeTarget::Found {
            session_path,
            cwd,
            session_kind,
            closed_at,
        } = target
        else {
            let _ = self.options.store.woken_end(durable_id, row.generation);
            return;
        };
        if session_kind != WakeSessionKind::Interactive {
            let _ = self.options.store.woken_end(durable_id, row.generation);
            return;
        }
        if !matches!(
            self.open_allowance(durable_id, &cwd, closed_at).await,
            Allowance::Open
        ) {
            let _ = self.options.store.woken_end(durable_id, row.generation);
            return;
        }
        if let OpenResult::Opened {
            connection,
            session_id,
            ..
        } = self.open(&session_path, &cwd).await
        {
            self.carry(durable_id, connection, session_id, row.generation);
            let outcome = self.evaluate_now(durable_id).await;
            self.emit(durable_id, &outcome);
        }
    }

    async fn sweep_now(self: &Arc<Self>) -> bool {
        if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return false;
        }
        let now = self.options.store.now();
        let rows = self.options.store.woken_list().unwrap_or_default();
        let count = rows.len();
        for row in rows {
            let is_carried = self
                .carried
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&row.durable_id);
            if is_carried {
                let _ = self.evaluate(&row.durable_id).await;
                continue;
            }
            if row.state == WokenSessionState::Requested {
                if row.next_attempt_at.is_none_or(|t| t <= now) {
                    let lock = self.lock_for(&row.durable_id);
                    let _g = lock.lock().await;
                    let _ = self.wake_now(&row.durable_id).await;
                }
                continue;
            }
            if row.lease_expires_at.is_some_and(|exp| exp <= now) {
                let lock = self.lock_for(&row.durable_id);
                let _g = lock.lock().await;
                self.take_over(&row.durable_id).await;
            }
        }
        count > 0
            || !self
                .carried
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
    }

    fn arm(self: &Arc<Self>) {
        if self.stopped.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        if self
            .timer_armed
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let this = self.clone();
        let interval_ms = CARRIER_RENEW_MS.min(WAKE_LEASE_MS / 3) as u64;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = this.sweep_wake.notified() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(interval_ms)) => {
                        if this.stopped.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }
                        if !this.sweep_now().await {
                            break;
                        }
                    }
                }
            }
            this.timer_armed
                .store(false, std::sync::atomic::Ordering::Release);
        });
    }

    pub async fn wake(self: &Arc<Self>, durable_id: &str) -> CarrierWakeOutcome {
        let lock = self.lock_for(durable_id);
        let _guard = lock.lock().await;
        self.arm();
        self.wake_now(durable_id).await
    }

    pub async fn sweep(self: &Arc<Self>) {
        let _ = self.sweep_now().await;
    }

    pub fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.sweep_wake.notify_waiters();
        let entries: Vec<String> = self
            .carried
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        for id in entries {
            self.forget(&id);
        }
    }
}
