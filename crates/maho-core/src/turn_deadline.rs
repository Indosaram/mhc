//! Port of senpi `packages/coding-agent/src/core/turn-deadline.ts`.
//!
//! The session-level turn deadline contract: how long a turn may run with no tool progress, and the
//! absolute cap on one turn's runtime (senpi#3227). Timers are tokio tasks; each arm/renew bumps a
//! generation so a superseded timer firing late does nothing.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

pub const TURN_DEADLINE_ENTRY_TYPE: &str = "deadline-stop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnDeadlineSettings {
    pub inactivity_ms: u64,
    pub max_runtime_ms: u64,
}

/// Milliseconds the abort is given to terminalize before the lease forces settlement.
pub const TURN_DEADLINE_GRACE_MS: u64 = 10_000;

pub const DEFAULT_TURN_INACTIVITY_MS: u64 = 3_600_000;
pub const MIN_TURN_INACTIVITY_MS: u64 = 60_000;
pub const DEFAULT_TURN_MAX_RUNTIME_MS: u64 = 21_600_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnDeadlineKind {
    Inactivity,
    MaxRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnDeadline {
    pub kind: TurnDeadlineKind,
    pub limit_ms: u64,
    pub elapsed_ms: u64,
}

/// The recorded form of a deadline stop, durable on the session branch so a config reload or session
/// reopen keeps suppressing goal continuations until admitted user input.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeadlineStopData {
    pub kind: TurnDeadlineKind,
    pub limit_ms: f64,
    pub elapsed_ms: f64,
    #[serde(default)]
    pub at: Option<f64>,
}

fn deadline_stop_data(data: &Value) -> Option<DeadlineStopData> {
    let object = data.as_object()?;
    let kind = match object.get("kind")?.as_str()? {
        "inactivity" => TurnDeadlineKind::Inactivity,
        "max_runtime" => TurnDeadlineKind::MaxRuntime,
        _ => return None,
    };
    Some(DeadlineStopData {
        kind,
        limit_ms: object.get("limitMs")?.as_f64()?,
        elapsed_ms: object.get("elapsedMs")?.as_f64()?,
        at: object.get("at").and_then(Value::as_f64),
    })
}

/// The latest deadline stop on the branch, or `None` when none is pending. Only a user message releases
/// the hold: the aborted assistant message of the stopped turn is persisted after the stop entry and
/// must not clear it.
pub fn deadline_stop_pending_since_last_turn(entries: &[Value]) -> Option<DeadlineStopData> {
    for entry in entries.iter().rev() {
        let entry_type = entry.get("type").and_then(Value::as_str);
        if entry_type == Some("message")
            && entry
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                == Some("user")
        {
            return None;
        }
        if entry_type == Some("custom")
            && entry.get("customType").and_then(Value::as_str) == Some(TURN_DEADLINE_ENTRY_TYPE)
            && let Some(data) = entry.get("data").and_then(deadline_stop_data)
        {
            return Some(data);
        }
    }
    None
}

pub fn resolve_turn_deadline_settings(
    turn_inactivity_ms: Option<&Value>,
    turn_max_runtime_ms: Option<&Value>,
) -> TurnDeadlineSettings {
    let milliseconds = |value: Option<&Value>, fallback: u64, minimum: u64| -> u64 {
        let Some(number) = value.and_then(Value::as_f64) else {
            return fallback;
        };
        if !number.is_finite() || number < 0.0 {
            return fallback;
        }
        if number == 0.0 {
            return 0;
        }
        minimum.max(number.floor() as u64)
    };
    TurnDeadlineSettings {
        inactivity_ms: milliseconds(
            turn_inactivity_ms,
            DEFAULT_TURN_INACTIVITY_MS,
            MIN_TURN_INACTIVITY_MS,
        ),
        max_runtime_ms: milliseconds(turn_max_runtime_ms, DEFAULT_TURN_MAX_RUNTIME_MS, 1),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnDeadlineWarning {
    RecordFailed,
    AbortFailed,
    Unsettled,
    Failed,
}

impl TurnDeadlineWarning {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecordFailed => "turn_deadline_record_failed",
            Self::AbortFailed => "turn_deadline_abort_failed",
            Self::Unsettled => "turn_deadline_unsettled",
            Self::Failed => "turn_deadline_failed",
        }
    }
}

pub type DeadlineAbort = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// The narrow AgentSession surface the tracker needs.
pub trait DeadlineSessionOwner: Send + Sync {
    fn record_deadline_stop(&self, deadline: &TurnDeadline) -> Result<(), String>;
    /// End the turn through the normal system abort; resolves once the session is idle.
    fn abort_turn_for_deadline(&self) -> DeadlineAbort;
    /// Stop waiting on extension handlers that still hold the turn; returns how many were released.
    fn release_held_extension_handlers(&self) -> usize;
    /// True while a provider retry is in flight; the engine is actively recovering the turn.
    fn is_retry_in_flight(&self) -> bool;
    fn is_idle(&self) -> bool;
    fn warn_deadline(&self, event: TurnDeadlineWarning, detail: &str);
}

pub type DeadlineClock = Arc<dyn Fn() -> u64 + Send + Sync>;
pub type DeadlineSettingsSource = Arc<dyn Fn() -> TurnDeadlineSettings + Send + Sync>;

#[derive(Default)]
struct TrackerState {
    deadline: Option<TurnDeadline>,
    started_at_ms: u64,
    inactivity_generation: u64,
    max_runtime_generation: u64,
    inactivity_timer: Option<tokio::task::JoinHandle<()>>,
    max_runtime_timer: Option<tokio::task::JoinHandle<()>>,
}

impl TrackerState {
    fn clear_timers(&mut self) {
        self.inactivity_generation += 1;
        self.max_runtime_generation += 1;
        if let Some(timer) = self.inactivity_timer.take() {
            timer.abort();
        }
        if let Some(timer) = self.max_runtime_timer.take() {
            timer.abort();
        }
    }
}

/// Owns the inactivity lease and the hard max-runtime cap for one turn. Armed at prompt accept, renewed
/// only by tool execution progress and admitted user input, never by text deltas. On expiry it records
/// the durable stop entry and ends the run through the normal system-abort path. `arm`/`renew` must be
/// called inside a tokio runtime.
#[derive(Clone)]
pub struct AgentDeadlineTracker {
    session: Arc<dyn DeadlineSessionOwner>,
    settings: DeadlineSettingsSource,
    now: DeadlineClock,
    state: Arc<Mutex<TrackerState>>,
}

fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

impl AgentDeadlineTracker {
    pub fn new(
        session: Arc<dyn DeadlineSessionOwner>,
        settings: DeadlineSettingsSource,
        now: Option<DeadlineClock>,
    ) -> Self {
        Self {
            session,
            settings,
            now: now.unwrap_or_else(|| Arc::new(system_now_ms)),
            state: Arc::new(Mutex::new(TrackerState::default())),
        }
    }

    pub fn current(&self) -> Option<TurnDeadline> {
        self.state.lock().ok().and_then(|state| state.deadline)
    }

    fn schedule(&self, state: &mut TrackerState, kind: TurnDeadlineKind, ms: u64) {
        let generation = match kind {
            TurnDeadlineKind::Inactivity => {
                state.inactivity_generation += 1;
                state.inactivity_generation
            }
            TurnDeadlineKind::MaxRuntime => {
                state.max_runtime_generation += 1;
                state.max_runtime_generation
            }
        };
        let tracker = self.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            tracker.fire(kind, generation).await;
        });
        let slot = match kind {
            TurnDeadlineKind::Inactivity => &mut state.inactivity_timer,
            TurnDeadlineKind::MaxRuntime => &mut state.max_runtime_timer,
        };
        if let Some(previous) = slot.replace(timer) {
            previous.abort();
        }
    }

    pub fn arm(&self) {
        let settings = (self.settings)();
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.clear_timers();
        state.deadline = None;
        state.started_at_ms = (self.now)().max(1);
        if settings.inactivity_ms > 0 {
            self.schedule(
                &mut state,
                TurnDeadlineKind::Inactivity,
                settings.inactivity_ms,
            );
        }
        if settings.max_runtime_ms > 0 {
            self.schedule(
                &mut state,
                TurnDeadlineKind::MaxRuntime,
                settings.max_runtime_ms,
            );
        }
    }

    pub fn renew(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        self.renew_locked(&mut state);
    }

    fn renew_locked(&self, state: &mut TrackerState) {
        if state.deadline.is_some() || state.started_at_ms == 0 {
            return;
        }
        let settings = (self.settings)();
        if settings.inactivity_ms == 0 {
            return;
        }
        self.schedule(state, TurnDeadlineKind::Inactivity, settings.inactivity_ms);
    }

    pub fn disarm(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.clear_timers();
            state.started_at_ms = 0;
        }
    }

    /// Admitted user input releases the deadline-stopped hold. Input steered into a live run counts as
    /// activity (the inactivity lease restarts) but never resets the max-runtime clock of that run.
    pub fn clear_hold(&self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.deadline.is_none() {
            self.renew_locked(&mut state);
            return;
        }
        if self.session.is_idle() {
            state.deadline = None;
        }
    }

    async fn fire(&self, kind: TurnDeadlineKind, generation: u64) {
        if let Err(error) = self.expire(kind, generation).await {
            self.session
                .warn_deadline(TurnDeadlineWarning::Failed, &error);
        }
    }

    async fn expire(&self, kind: TurnDeadlineKind, generation: u64) -> Result<(), String> {
        let deadline = {
            let mut state = self.state.lock().map_err(|error| error.to_string())?;
            let current_generation = match kind {
                TurnDeadlineKind::Inactivity => state.inactivity_generation,
                TurnDeadlineKind::MaxRuntime => state.max_runtime_generation,
            };
            if current_generation != generation
                || state.deadline.is_some()
                || state.started_at_ms == 0
            {
                return Ok(());
            }
            let settings = (self.settings)();
            let limit_ms = match kind {
                TurnDeadlineKind::Inactivity => settings.inactivity_ms,
                TurnDeadlineKind::MaxRuntime => settings.max_runtime_ms,
            };
            if limit_ms == 0 {
                return Ok(());
            }
            // A provider retry in flight is the engine recovering the turn: it renews the inactivity lease.
            // The max-runtime cap is absolute and applies through retries.
            if kind == TurnDeadlineKind::Inactivity && self.session.is_retry_in_flight() {
                state.inactivity_timer = None;
                self.renew_locked(&mut state);
                return Ok(());
            }
            // This task is one of the timers: detach it before clearing so it is not aborted mid-expiry.
            match kind {
                TurnDeadlineKind::Inactivity => state.inactivity_timer = None,
                TurnDeadlineKind::MaxRuntime => state.max_runtime_timer = None,
            }
            state.clear_timers();
            let deadline = TurnDeadline {
                kind,
                limit_ms,
                elapsed_ms: (self.now)().saturating_sub(state.started_at_ms),
            };
            state.deadline = Some(deadline);
            deadline
        };
        if let Err(error) = self.session.record_deadline_stop(&deadline) {
            // The stop still happens; only its durable hold entry is missing.
            self.session
                .warn_deadline(TurnDeadlineWarning::RecordFailed, &error);
        }
        let abort_started_at = (self.now)();
        let mut aborting = tokio::spawn(self.session.abort_turn_for_deadline());
        let grace = Duration::from_millis(TURN_DEADLINE_GRACE_MS);
        let mut outcome = tokio::time::timeout(grace, &mut aborting).await.ok();
        if outcome.is_none() {
            // A turn held by something no abort can end: stop waiting on held handlers, one more grace.
            self.session.release_held_extension_handlers();
            outcome = tokio::time::timeout(grace, &mut aborting).await.ok();
        }
        let outcome = outcome.map(|joined| {
            joined
                .map_err(|error| error.to_string())
                .and_then(|result| result)
        });
        let abort_error = match &outcome {
            Some(Err(error)) => Some(error.clone()),
            _ => None,
        };
        if let Some(error) = &abort_error {
            self.session
                .warn_deadline(TurnDeadlineWarning::AbortFailed, error);
        }
        if outcome.is_some() && abort_error.is_none() {
            return Ok(());
        }
        if !self.session.is_idle() {
            let kind_name = match kind {
                TurnDeadlineKind::Inactivity => "inactivity",
                TurnDeadlineKind::MaxRuntime => "max_runtime",
            };
            self.session.warn_deadline(
                TurnDeadlineWarning::Unsettled,
                &format!(
                    "turn still running {} ms after the {kind_name} deadline abort",
                    (self.now)().saturating_sub(abort_started_at)
                ),
            );
        }
        Ok(())
    }
}

pub fn build_deadline_stop_notice(deadline: &TurnDeadline) -> String {
    let limit = if deadline.limit_ms < 60_000 {
        format!(
            "{} s",
            ((deadline.limit_ms as f64 / 1000.0).round() as u64).max(1)
        )
    } else {
        format!(
            "{} min",
            (deadline.limit_ms as f64 / 60_000.0).round() as u64
        )
    };
    let reason = match deadline.kind {
        TurnDeadlineKind::Inactivity => {
            format!("no tool progress for {limit} (turn_inactivity_ms)")
        }
        TurnDeadlineKind::MaxRuntime => {
            format!("turn hit its maximum runtime of {limit} (turn_max_runtime_ms)")
        }
    };
    format!(
        "turn stopped: {reason}. Action: send a message to continue, or raise the limit in settings"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[test]
    fn settings_clamp_to_minimums_and_keep_zero_as_disabled() {
        assert_eq!(
            resolve_turn_deadline_settings(None, None),
            TurnDeadlineSettings {
                inactivity_ms: DEFAULT_TURN_INACTIVITY_MS,
                max_runtime_ms: DEFAULT_TURN_MAX_RUNTIME_MS
            }
        );
        let settings = resolve_turn_deadline_settings(Some(&json!(10)), Some(&json!(0)));
        assert_eq!(
            settings,
            TurnDeadlineSettings {
                inactivity_ms: MIN_TURN_INACTIVITY_MS,
                max_runtime_ms: 0
            }
        );
        let settings = resolve_turn_deadline_settings(Some(&json!(-5)), Some(&json!("x")));
        assert_eq!(settings.inactivity_ms, DEFAULT_TURN_INACTIVITY_MS);
        assert_eq!(settings.max_runtime_ms, DEFAULT_TURN_MAX_RUNTIME_MS);
        assert_eq!(
            resolve_turn_deadline_settings(Some(&json!(90_000.7)), None).inactivity_ms,
            90_000
        );
    }

    #[test]
    fn only_a_user_message_releases_a_pending_stop() {
        let stop = json!({"type":"custom","customType":"deadline-stop","data":{"kind":"inactivity","limitMs":60000,"elapsedMs":61000,"at":1}});
        let aborted = json!({"type":"message","message":{"role":"assistant"}});
        let user = json!({"type":"message","message":{"role":"user"}});
        let pending = deadline_stop_pending_since_last_turn(&[stop.clone(), aborted]);
        assert_eq!(
            pending.map(|data| data.kind),
            Some(TurnDeadlineKind::Inactivity)
        );
        assert!(deadline_stop_pending_since_last_turn(&[stop.clone(), user]).is_none());
        let malformed =
            json!({"type":"custom","customType":"deadline-stop","data":{"kind":"other"}});
        assert!(deadline_stop_pending_since_last_turn(&[malformed]).is_none());
    }

    #[test]
    fn notices_name_the_limit_and_its_setting() {
        assert_eq!(
            build_deadline_stop_notice(&TurnDeadline {
                kind: TurnDeadlineKind::Inactivity,
                limit_ms: 3_600_000,
                elapsed_ms: 0
            }),
            "turn stopped: no tool progress for 60 min (turn_inactivity_ms). Action: send a message to continue, or raise the limit in settings"
        );
        assert_eq!(
            build_deadline_stop_notice(&TurnDeadline {
                kind: TurnDeadlineKind::MaxRuntime,
                limit_ms: 400,
                elapsed_ms: 0
            }),
            "turn stopped: turn hit its maximum runtime of 1 s (turn_max_runtime_ms). Action: send a message to continue, or raise the limit in settings"
        );
    }

    struct FakeSession {
        recorded: tokio::sync::Notify,
        records: AtomicUsize,
        retrying: AtomicBool,
        warnings: Mutex<Vec<&'static str>>,
    }

    impl DeadlineSessionOwner for FakeSession {
        fn record_deadline_stop(&self, _deadline: &TurnDeadline) -> Result<(), String> {
            self.records.fetch_add(1, Ordering::SeqCst);
            self.recorded.notify_one();
            Ok(())
        }
        fn abort_turn_for_deadline(&self) -> DeadlineAbort {
            Box::pin(async { Ok(()) })
        }
        fn release_held_extension_handlers(&self) -> usize {
            0
        }
        fn is_retry_in_flight(&self) -> bool {
            self.retrying.load(Ordering::SeqCst)
        }
        fn is_idle(&self) -> bool {
            true
        }
        fn warn_deadline(&self, event: TurnDeadlineWarning, _detail: &str) {
            if let Ok(mut warnings) = self.warnings.lock() {
                warnings.push(event.as_str());
            }
        }
    }

    fn fake() -> Arc<FakeSession> {
        Arc::new(FakeSession {
            recorded: tokio::sync::Notify::new(),
            records: AtomicUsize::new(0),
            retrying: AtomicBool::new(false),
            warnings: Mutex::new(Vec::new()),
        })
    }

    #[tokio::test]
    async fn an_idle_turn_records_one_inactivity_stop() {
        let session = fake();
        let tracker = AgentDeadlineTracker::new(
            session.clone(),
            Arc::new(|| TurnDeadlineSettings {
                inactivity_ms: 20,
                max_runtime_ms: 0,
            }),
            None,
        );
        let recorded = session.recorded.notified();
        tracker.arm();
        assert!(
            tokio::time::timeout(Duration::from_secs(10), recorded)
                .await
                .is_ok()
        );
        assert_eq!(
            tracker.current().map(|deadline| deadline.kind),
            Some(TurnDeadlineKind::Inactivity)
        );
        assert_eq!(session.records.load(Ordering::SeqCst), 1);
        tracker.clear_hold();
        assert_eq!(tracker.current(), None);
    }

    #[tokio::test]
    async fn the_max_runtime_cap_fires_through_retries() {
        let session = fake();
        session.retrying.store(true, Ordering::SeqCst);
        let tracker = AgentDeadlineTracker::new(
            session.clone(),
            Arc::new(|| TurnDeadlineSettings {
                inactivity_ms: 10,
                max_runtime_ms: 50,
            }),
            None,
        );
        let recorded = session.recorded.notified();
        tracker.arm();
        assert!(
            tokio::time::timeout(Duration::from_secs(10), recorded)
                .await
                .is_ok()
        );
        assert_eq!(
            tracker.current().map(|deadline| deadline.kind),
            Some(TurnDeadlineKind::MaxRuntime)
        );
        assert!(
            session
                .warnings
                .lock()
                .map(|warnings| warnings.is_empty())
                .unwrap_or(false)
        );
    }
}
