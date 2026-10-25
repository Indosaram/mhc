//! Port of `packages/omo-senpi/src/components/telemetry/omo-native-session-summary.ts`.
//!
//! A durable, owner-aware recorder: every window is a file stamped with the writing process's owner record
//! and consent scope, every close writes the exact event to `pending/` before advancing the sequence, and the
//! pending file is retired only once the transport acknowledged it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use telemetry_core::{TelemetryDiagnosticEvent, TelemetryDiagnostics, TelemetryError};

use crate::session_role::TelemetrySessionRole;
use crate::session_summary_adopt::FreshArgs;
use crate::session_summary_lazy_recovery::LazySummaryRecovery;
use crate::session_summary_owner::Clock;
use crate::session_summary_publisher::{ClosedWindow, SummaryPublish, SummaryPublisher};
use crate::session_summary_sampling::is_session_sampled;
use crate::session_summary_store::{
    ConsentScope, ReadResult, RecordConsent, SummaryStorage, WindowRecord, is_record_sendable,
};
use crate::session_summary_window::{
    EndedBy, SummaryTurn, SummaryWindow, add_turn, new_summary_epoch, open_window,
};

pub const CHECKPOINT_MAX_TURNS: usize = 50;
pub const CHECKPOINT_MAX_WINDOW_MS: f64 = 30.0 * 60_000.0;
pub const HEARTBEAT_INTERVAL_MS: u64 = 60_000;
/// How often a persisted window re-checks consent, so an opt-out deletes it even if no turn follows.
pub const CONSENT_CHECK_INTERVAL_MS: u64 = 5_000;

/// One session's open numbering state.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWindow {
    pub key: String,
    pub epoch: String,
    pub seq: u64,
    pub sample_rate: f64,
    pub window_id: Option<String>,
    pub window: Option<SummaryWindow>,
}

pub struct SessionSummaryOptions {
    /// Asked before every write, because consent can change mid-session; `true` is a fresh read.
    pub is_consented: Arc<dyn Fn(bool) -> bool + Send + Sync>,
    /// The scope `is_consented` reads right now; stamped into every persisted record.
    pub consent_scope: Arc<dyn Fn() -> ConsentScope + Send + Sync>,
    /// Consent for the scope a persisted record came from, read when it is about to be sent or resumed.
    pub is_scope_consented: Arc<dyn Fn(&ConsentScope) -> bool + Send + Sync>,
    pub storage: SummaryStorage,
    pub publish: SummaryPublish,
    pub session_role: TelemetrySessionRole,
    /// Read when a window opens; the component resolves config, env and sink lazily behind it.
    pub sample_rate: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub now: Option<Clock>,
    pub diagnostics: Option<TelemetryDiagnostics>,
    pub dropped_events: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
    pub is_pid_alive: Option<crate::session_summary_liveness::PidProbe>,
}

#[derive(Default)]
struct State {
    live: BTreeMap<String, LiveWindow>,
    /// Window files this process wrote, by session.
    on_disk: BTreeMap<String, String>,
    swept: bool,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
    consent_check: Option<tokio::task::JoinHandle<()>>,
}

pub struct SessionSummaryRecorder {
    state: Arc<Mutex<State>>,
    options: SessionSummaryOptions,
    storage: SummaryStorage,
    publisher: Arc<SummaryPublisher>,
    recovery: Arc<LazySummaryRecovery>,
    now: Clock,
    /// A weak self-reference, so the periodic tasks never keep the recorder alive.
    this: Mutex<Weak<Self>>,
}

impl Drop for SessionSummaryRecorder {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = state.heartbeat.take() {
            handle.abort();
        }
        if let Some(handle) = state.consent_check.take() {
            handle.abort();
        }
    }
}

impl SessionSummaryRecorder {
    pub fn new(options: SessionSummaryOptions) -> Arc<Self> {
        let storage = options.storage.clone();
        let now = options
            .now
            .clone()
            .unwrap_or_else(|| Arc::clone(&storage.now));
        let publisher = Arc::new(SummaryPublisher::new(
            storage.clone(),
            Arc::clone(&options.publish),
            options.diagnostics.clone(),
            options.dropped_events.clone(),
            Arc::clone(&options.is_scope_consented),
        ));
        let state: Arc<Mutex<State>> = Arc::default();
        let own_state = Arc::clone(&state);
        let recovery = Arc::new(LazySummaryRecovery::new(
            storage.clone(),
            Arc::clone(&publisher),
            Arc::clone(&options.is_scope_consented),
            options.diagnostics.clone(),
            Arc::new(move || {
                own_state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .on_disk
                    .values()
                    .cloned()
                    .collect()
            }),
        ));
        let recorder = Arc::new(Self {
            state,
            options,
            storage,
            publisher,
            recovery,
            now,
            this: Mutex::new(Weak::new()),
        });
        *recorder
            .this
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(&recorder);
        recorder
    }

    fn this(&self) -> Option<Arc<Self>> {
        self.this
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade()
    }

    pub fn sample_rate(&self) -> f64 {
        (self.options.sample_rate)()
    }

    pub fn is_raw_turn_sampled(&self, session_hash: &str) -> bool {
        is_session_sampled(session_hash, self.sample_rate())
    }

    fn consented(&self) -> bool {
        (self.options.is_consented)(false)
    }

    fn consented_fresh(&self) -> bool {
        (self.options.is_consented)(true)
    }

    fn report(&self, error: TelemetryError) {
        self.publisher.report(
            TelemetryDiagnosticEvent::TelemetryActivityStateWriteFailed,
            Some(error),
            0,
        );
    }

    fn track(&self, session_hash: &str, key: Option<&str>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match key {
            None => {
                state.on_disk.remove(session_hash);
            }
            Some(key) => {
                state
                    .on_disk
                    .insert(session_hash.to_string(), key.to_string());
            }
        }
    }

    /// Armed from the first turn: a window on disk must be deleted if the user opts out and no turn follows.
    fn ensure_consent_check(&self) {
        {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.consent_check.is_some() {
                return;
            }
        }
        let Some(this) = self.this() else {
            return;
        };
        let weak = Arc::downgrade(&this);
        let handle = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(CONSENT_CHECK_INTERVAL_MS));
            interval.tick().await;
            loop {
                interval.tick().await;
                let Some(recorder) = weak.upgrade() else {
                    break;
                };
                recorder.purge_if_opted_out();
            }
        });
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .consent_check = Some(handle);
    }

    fn disarm_consent_check(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = state.consent_check.take() {
            handle.abort();
        }
    }

    fn remove_window(&self, session_hash: &str, key: &str) {
        self.storage.remove_window(key);
        let still_tracked = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_disk
            .get(session_hash)
            .is_some_and(|tracked| tracked == key);
        if still_tracked {
            self.track(session_hash, None);
        }
    }

    fn record_of(&self, session_hash: &str, entry: &LiveWindow) -> WindowRecord {
        WindowRecord {
            consent: RecordConsent::Enabled,
            scope: Some((self.options.consent_scope)()),
            session_hash: session_hash.to_string(),
            session_role: self.options.session_role,
            owner: self.storage.owner.clone(),
            heartbeat_at: (self.now)(),
            epoch: Some(entry.epoch.clone()),
            seq: entry.seq,
            sample_rate: entry.sample_rate,
            window_id: entry.window_id.clone(),
            window: entry.window.clone(),
        }
    }

    fn persist(&self, session_hash: &str, entry: &LiveWindow) {
        let record = self.record_of(session_hash, entry);
        match self.storage.write_window(&entry.key, &record) {
            Ok(()) => self.track(session_hash, Some(&entry.key)),
            Err(error) => self.report(TelemetryError::new(error.to_string())),
        }
    }

    /// Opting out drops the session with nothing left on disk: its live state and its window file. Opting
    /// back in starts a new epoch, so no (epoch, seq) pair it already sent can repeat.
    fn discard(&self, session_hash: &str) {
        let key = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let key = state
                .live
                .get(session_hash)
                .map(|entry| entry.key.clone())
                .or_else(|| state.on_disk.get(session_hash).cloned());
            state.live.remove(session_hash);
            key
        };
        if let Some(key) = key {
            self.remove_window(session_hash, &key);
        }
    }

    /// Re-reads consent and, when it is off, deletes every window this process left on disk.
    pub fn purge_if_opted_out(&self) {
        let (has_disk, has_live, sessions) = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let mut sessions: Vec<String> = state.on_disk.keys().cloned().collect();
            sessions.extend(state.live.keys().cloned());
            sessions.sort();
            sessions.dedup();
            (!state.on_disk.is_empty(), !state.live.is_empty(), sessions)
        };
        if !has_disk && !has_live {
            return;
        }
        if self.consented_fresh() {
            return;
        }
        for session_hash in sessions {
            self.discard(&session_hash);
        }
    }

    fn ensure_heartbeat(&self) {
        {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.heartbeat.is_some() {
                return;
            }
        }
        let Some(this) = self.this() else {
            return;
        };
        let weak = Arc::downgrade(&this);
        let handle = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_millis(HEARTBEAT_INTERVAL_MS));
            interval.tick().await;
            loop {
                interval.tick().await;
                let Some(recorder) = weak.upgrade() else {
                    break;
                };
                recorder.heartbeat().await;
            }
        });
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .heartbeat = Some(handle);
    }

    fn stop_heartbeat(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = state.heartbeat.take() {
            handle.abort();
        }
    }

    async fn heartbeat(&self) {
        self.purge_if_opted_out();
        if !self.consented() {
            return;
        }
        // An idle session writes no window, so the tick itself keeps this owner's entry on disk.
        self.storage.register();
        let open: Vec<(String, LiveWindow)> = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state
                .live
                .iter()
                .filter(|(_, entry)| entry.window.is_some())
                .map(|(hash, entry)| (hash.clone(), entry.clone()))
                .collect()
        };
        for (session_hash, entry) in open {
            self.persist(&session_hash, &entry);
        }
        self.recovery.sweep().await;
    }

    async fn sweep_once(&self) {
        let first = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.swept {
                false
            } else {
                state.swept = true;
                true
            }
        };
        if first {
            self.recovery.sweep().await;
        }
    }

    fn is_own(&self, owner: &crate::session_summary_owner::ProcessOwner) -> bool {
        owner.pid == self.storage.owner.pid && owner.boot_id == self.storage.owner.boot_id
    }

    /// Before a session adopts a window another process wrote, the liveness side must be built.
    fn load_liveness_for(&self, session_hash: &str) {
        {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if state.live.contains_key(session_hash) {
                return;
            }
        }
        if self.recovery.runtime().is_some() {
            return;
        }
        if let ReadResult::Value(stored) = self
            .storage
            .read_window(&self.storage.window_key(session_hash, false))
            && !self.is_own(&stored.owner)
        {
            let _runtime = self.recovery.load();
        }
    }

    /// What a turn or shutdown waits for first: the first sweep, then the liveness check it may need.
    async fn ready(&self, session_hash: &str) {
        self.sweep_once().await;
        self.load_liveness_for(session_hash);
    }

    /// A session's file is continued by whoever records its next turn: this process's own file, or the file
    /// of a dead owner (claimed first, so two resuming processes never both continue it). Continuing a file
    /// keeps its epoch and seq; anything else starts a new epoch at seq 0, so an own copy beside another
    /// process's window can never publish that window's (epoch, seq).
    fn adopt(&self, session_hash: &str) -> LiveWindow {
        let key = self.storage.window_key(session_hash, false);
        let own_copy = self.storage.window_key(session_hash, true);
        let fresh = |args: FreshArgs| LiveWindow {
            key: args.key.unwrap_or_else(|| key.clone()),
            epoch: args.epoch.unwrap_or_else(new_summary_epoch),
            seq: args.seq.unwrap_or(0),
            sample_rate: self.sample_rate(),
            window_id: None,
            window: None,
        };
        let stored = match self.storage.read_window(&key) {
            ReadResult::Value(stored) => stored,
            ReadResult::Missing | ReadResult::Corrupt => return fresh(FreshArgs::default()),
        };
        // Only a window marked opted in, whose originating scope is still opted in now, may be continued.
        if !is_record_sendable(
            stored.consent,
            stored.scope.as_ref(),
            self.options.is_scope_consented.as_ref(),
        ) {
            self.storage.remove_window(&key);
            return fresh(FreshArgs::default());
        }
        if self.is_own(&stored.owner) {
            return LiveWindow {
                key: key.clone(),
                epoch: stored.epoch.unwrap_or_else(new_summary_epoch),
                seq: stored.seq,
                sample_rate: stored.sample_rate,
                window_id: stored.window_id,
                window: stored.window,
            };
        }
        // Without the liveness side the owner counts as alive: nothing is claimed.
        match self.recovery.runtime() {
            Some(runtime) => runtime.recovery.adopt(
                &key,
                &own_copy,
                &stored,
                self.options.is_scope_consented.as_ref(),
                &fresh,
                &|entry: &LiveWindow| self.persist(session_hash, entry),
            ),
            None => fresh(FreshArgs {
                key: Some(own_copy),
                ..Default::default()
            }),
        }
    }

    fn live_for(&self, session_hash: &str) -> LiveWindow {
        {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(entry) = state.live.get(session_hash) {
                return entry.clone();
            }
        }
        let entry = self.adopt(session_hash);
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .insert(session_hash.to_string(), entry.clone());
        entry
    }

    async fn close(&self, session_hash: &str, entry: LiveWindow, ended_by: EndedBy, keep_file: bool) {
        let window = entry
            .window
            .clone()
            .unwrap_or_else(|| open_window((self.now)(), 0.0));
        let window_id = entry
            .window_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let seq = entry.seq;
        let closed = ClosedWindow {
            consent: RecordConsent::Enabled,
            scope: Some((self.options.consent_scope)()),
            session_hash: session_hash.to_string(),
            session_role: self.options.session_role,
            sample_rate: entry.sample_rate,
            epoch: entry.epoch.clone(),
            seq,
            window_id,
            window,
            ended_by,
        };
        let advance = move || {
            let mut updated = entry.clone();
            updated.seq = seq + 1;
            updated.window = None;
            updated.window_id = None;
            {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.live.contains_key(session_hash) {
                    state.live.insert(session_hash.to_string(), updated.clone());
                }
            }
            if keep_file {
                self.persist(session_hash, &updated);
            } else {
                self.remove_window(session_hash, &updated.key);
            }
        };
        self.publisher.publish_window(&closed, advance).await;
    }

    /// Re-sends unacknowledged events (this process's and dead ones') and recovers abandoned windows.
    pub async fn recover(&self) {
        if !self.consented() {
            return;
        }
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.swept = true;
        }
        self.recovery.sweep().await;
        self.publisher.settled().await;
    }

    pub async fn record_turn(&self, session_hash: &str, turn: SummaryTurn) {
        if !self.consented() {
            self.discard(session_hash);
            return;
        }
        self.ready(session_hash).await;
        self.ensure_heartbeat();
        self.ensure_consent_check();
        let mut entry = self.live_for(session_hash);
        let at = (self.now)();
        if entry.window.is_none() {
            entry.window_id = Some(uuid::Uuid::new_v4().to_string());
            entry.window = Some(open_window(at, turn.turn_index));
            entry.sample_rate = self.sample_rate();
        }
        if let Some(window) = entry.window.as_mut() {
            add_turn(window, &turn, at);
        }
        let checkpoint = entry.window.as_ref().is_some_and(|window| {
            window.totals.len() >= CHECKPOINT_MAX_TURNS
                || at - window.started_at >= CHECKPOINT_MAX_WINDOW_MS
        });
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live
            .insert(session_hash.to_string(), entry.clone());
        if checkpoint {
            self.close(session_hash, entry, EndedBy::Checkpoint, true)
                .await;
        } else {
            self.persist(session_hash, &entry);
        }
        self.publisher.settled().await;
    }

    /// `reload` keeps the session's sequence so the next registration continues it.
    pub async fn finish(&self, session_hash: &str, reason: Option<&str>) {
        if !self.consented() {
            self.discard(session_hash);
            return;
        }
        self.ready(session_hash).await;
        self.ensure_consent_check();
        let entry = self.live_for(session_hash);
        let last = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.live.remove(session_hash);
            state.live.is_empty()
        };
        if last {
            self.stop_heartbeat();
        }
        let own_copy = entry.key != self.storage.window_key(session_hash, false);
        if reason == Some("reload") {
            if entry.window.is_some() {
                self.close(session_hash, entry, EndedBy::Reload, !own_copy)
                    .await;
            } else if entry.seq > 0 && !own_copy {
                self.persist(session_hash, &entry);
            }
        } else if entry.window.is_some() || entry.seq > 0 {
            self.close(session_hash, entry, EndedBy::Shutdown, !own_copy)
                .await;
        } else {
            self.remove_window(session_hash, &entry.key);
        }
        self.publisher.settled().await;
    }

    /// Stops the periodic consent check; the session is going away.
    pub fn dispose(&self) {
        self.disarm_consent_check();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_summary_delivery::SummaryPublication;
    use crate::session_summary_store::{Probes, create_summary_storage};
    use futures::FutureExt;
    use futures::future::BoxFuture;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    struct Fixture {
        dir: tempfile::TempDir,
        consented: Arc<AtomicBool>,
        sent: Arc<Mutex<Vec<SummaryPublication>>>,
    }

    fn fixture() -> Fixture {
        Fixture {
            dir: tempfile::tempdir().unwrap(),
            consented: Arc::new(AtomicBool::new(true)),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn options(fixture: &Fixture) -> SessionSummaryOptions {
        let consented = Arc::clone(&fixture.consented);
        let sent = Arc::clone(&fixture.sent);
        let publish: SummaryPublish = Arc::new(move |publication| {
            sent.lock().unwrap().push(publication);
            async { Ok(()) }.boxed() as BoxFuture<'static, _>
        });
        let storage = create_summary_storage(
            fixture.dir.path(),
            crate::session_summary_owner::ProcessOwner {
                pid: std::process::id(),
                boot_id: "boot-self".into(),
            },
            Probes {
                is_pid_alive: Some(Arc::new(|_| false)),
                process_start_time: Some(Arc::new(|_| None)),
            },
            Some(Arc::new(|| 1000.0)),
        );
        SessionSummaryOptions {
            is_consented: Arc::new(move |_| consented.load(Ordering::SeqCst)),
            consent_scope: Arc::new(|| ConsentScope {
                cwd: Some("/repo".into()),
                config: None,
            }),
            is_scope_consented: Arc::new(|_| true),
            storage,
            publish,
            session_role: TelemetrySessionRole::Root,
            sample_rate: Arc::new(|| 0.05),
            now: Some(Arc::new(|| 1000.0)),
            diagnostics: None,
            dropped_events: None,
            is_pid_alive: None,
        }
    }

    fn recorder(fixture: &Fixture) -> Arc<SessionSummaryRecorder> {
        SessionSummaryRecorder::new(options(fixture))
    }

    fn turn(index: u64) -> SummaryTurn {
        SummaryTurn {
            turn_index: index as f64,
            provider: "openai".into(),
            model_id: "gpt-5.6-sol".into(),
            input_tokens: 1.0,
            output_tokens: 1.0,
            cache_read_tokens: 0.0,
            cache_write_tokens: 0.0,
            reasoning_tokens: 0.0,
            total_tokens: 2.0,
            cost_1e4: 1.0,
            usage_valid: true,
            stop_reason: None,
            tool_result_count: 0.0,
            tool_error_count: 0.0,
        }
    }

    fn window_path(fixture: &Fixture, hash: &str) -> std::path::PathBuf {
        fixture
            .dir
            .path()
            .join("session-summary")
            .join(format!("{hash}.json"))
    }

    fn stored(fixture: &Fixture, hash: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(window_path(fixture, hash)).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn a_turn_persists_a_window_and_a_checkpoint_publishes_it() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        let hash = "a".repeat(64);
        recorder.record_turn(&hash, turn(1)).await;
        assert!(window_path(&fixture, &hash).exists());
        assert!(fixture.sent.lock().unwrap().is_empty());
        for index in 2..=50 {
            recorder.record_turn(&hash, turn(index)).await;
        }
        let sent = fixture.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].properties["turns"], 50);
        assert_eq!(sent[0].properties["ended_by"], "checkpoint");
        assert_eq!(sent[0].properties["cost_1e4"], 50.0);
        assert_eq!(sent[0].properties["cost_usd"], 0.005);
        assert_eq!(sent[0].properties["session_role"], "root");
        assert_eq!(sent[0].uuid.len(), 36);
    }

    #[tokio::test]
    async fn shutdown_publishes_a_final_window_and_advances_the_file() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        let hash = "b".repeat(64);
        recorder.record_turn(&hash, turn(1)).await;
        recorder.finish(&hash, Some("shutdown")).await;
        let sent = fixture.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].properties["is_final"], true);
        assert_eq!(sent[0].properties["ended_by"], "shutdown");
        assert_eq!(sent[0].properties["summary_seq"], 0);
        drop(sent);
        let stored = stored(&fixture, &hash);
        assert_eq!(stored["seq"], 1);
        assert!(stored["window"].is_null());
    }

    #[tokio::test]
    async fn reload_keeps_the_sequence_and_the_window_for_the_next_registration() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        let hash = "c".repeat(64);
        recorder.record_turn(&hash, turn(1)).await;
        recorder.finish(&hash, Some("reload")).await;
        assert_eq!(fixture.sent.lock().unwrap()[0].properties["ended_by"], "reload");
        assert_eq!(stored(&fixture, &hash)["seq"], 1);
        recorder.record_turn(&hash, turn(2)).await;
        recorder.finish(&hash, Some("shutdown")).await;
        assert_eq!(fixture.sent.lock().unwrap()[1].properties["summary_seq"], 1);
    }

    #[tokio::test]
    async fn opting_out_discards_the_window_and_sends_nothing() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        let hash = "d".repeat(64);
        recorder.record_turn(&hash, turn(1)).await;
        fixture.consented.store(false, Ordering::SeqCst);
        recorder.record_turn(&hash, turn(2)).await;
        assert!(!window_path(&fixture, &hash).exists());
        assert!(fixture.sent.lock().unwrap().is_empty());
        recorder.finish(&hash, Some("shutdown")).await;
        assert!(fixture.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn purge_deletes_windows_written_while_consented() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        let hash = "e".repeat(64);
        recorder.record_turn(&hash, turn(1)).await;
        recorder.purge_if_opted_out();
        assert!(window_path(&fixture, &hash).exists());
        fixture.consented.store(false, Ordering::SeqCst);
        recorder.purge_if_opted_out();
        assert!(!window_path(&fixture, &hash).exists());
    }

    #[tokio::test]
    async fn raw_turn_sampling_follows_the_configured_rate() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        assert_eq!(recorder.sample_rate(), 0.05);
        let sampled = (0..200)
            .filter(|index| recorder.is_raw_turn_sampled(&format!("session-{index}")))
            .count();
        assert!((1..60).contains(&sampled), "sampled {sampled}");
    }

    #[tokio::test]
    async fn recovery_publishes_an_abandoned_foreign_window() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        let hash = "f".repeat(64);
        let path = window_path(&fixture, &hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            json!({
                "v":2,"consent":"enabled","scope":{"cwd":"/repo"},
                "sessionHash":hash,"sessionRole":"child",
                "owner":{"pid":987654,"bootId":"boot-dead"},
                "heartbeatAt":-1000000.0,"epoch":"0123456789abcdef","seq":7,"sampleRate":0.05,
                "windowId":"01234567-89ab-cdef-0123-456789abcdef",
                "window":{"startedAt":0.0,"lastAt":10.0,"firstTurnIndex":1.0,"lastTurnIndex":1.0,
                    "sums":{"total_tokens":2.0,"cost_1e4":1.0},"totals":[2.0],
                    "models":[{"provider":"openai","model":"gpt-5.6-sol","sums":{"turns":1.0}}]}
            })
            .to_string(),
        )
        .unwrap();
        recorder.recover().await;
        let sent = fixture.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].properties["ended_by"], "recovered");
        assert_eq!(sent[0].properties["summary_seq"], 7);
        assert_eq!(sent[0].properties["session_role"], "child");
        assert_eq!(sent[0].uuid, "01234567-89ab-cdef-0123-456789abcdef");
    }

    #[tokio::test]
    async fn a_dead_owners_open_window_is_continued_with_a_new_epoch() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        let hash = "g".repeat(64);
        let path = window_path(&fixture, &hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            json!({
                "v":2,"consent":"enabled","scope":{"cwd":"/repo"},
                "sessionHash":hash,"sessionRole":"root",
                "owner":{"pid":987655,"bootId":"boot-other"},
                "heartbeatAt":1000.0,"epoch":"0123456789abcdef","seq":3,"sampleRate":0.05,
                "windowId":null,"window":null
            })
            .to_string(),
        )
        .unwrap();
        recorder.record_turn(&hash, turn(1)).await;
        let stored = stored(&fixture, &hash);
        assert_eq!(stored["seq"], 3);
        assert_eq!(stored["owner"]["bootId"], "boot-self");
        assert_ne!(stored["epoch"], "0123456789abcdef");
        assert_eq!(stored["window"]["totals"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn continued_numbering_survives_a_restart() {
        let fixture = fixture();
        let hash = "h".repeat(64);
        {
            let recorder = recorder(&fixture);
            recorder.record_turn(&hash, turn(1)).await;
            recorder.finish(&hash, Some("reload")).await;
        }
        let recorder = recorder(&fixture);
        recorder.record_turn(&hash, turn(2)).await;
        recorder.finish(&hash, Some("shutdown")).await;
        let sent = fixture.sent.lock().unwrap();
        assert_eq!(sent[0].properties["summary_seq"], 0);
        assert_eq!(sent[1].properties["summary_seq"], 1);
        assert_eq!(
            sent[0].properties["summary_epoch"],
            sent[1].properties["summary_epoch"]
        );
    }

    #[tokio::test]
    async fn counters_are_per_window() {
        let fixture = fixture();
        let recorder = recorder(&fixture);
        let hash = "i".repeat(64);
        let mut failed = turn(1);
        failed.stop_reason = Some("error".into());
        failed.usage_valid = false;
        recorder.record_turn(&hash, failed).await;
        recorder.finish(&hash, Some("shutdown")).await;
        let sent = fixture.sent.lock().unwrap();
        assert_eq!(sent[0].properties["error_turns"], 1.0);
        assert_eq!(sent[0].properties["invalid_usage_turns"], 1.0);
        assert_eq!(sent[0].properties["turns"], 1);
    }

    #[tokio::test]
    async fn dropped_counters_are_reported_once_per_publication() {
        let fixture = fixture();
        let dropped = Arc::new(AtomicU64::new(2));
        let observed = Arc::clone(&dropped);
        let mut options = options(&fixture);
        options.dropped_events = Some(Arc::new(move || observed.load(Ordering::SeqCst)));
        let recorder = SessionSummaryRecorder::new(options);
        recorder.record_turn(&"j".repeat(64), turn(1)).await;
        recorder.finish(&"j".repeat(64), Some("shutdown")).await;
        let sent = fixture.sent.lock().unwrap();
        assert_eq!(sent[0].properties["dropped_events"], 2);
    }
}
