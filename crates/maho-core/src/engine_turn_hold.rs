//! Port of senpi packages/coding-agent/src/core/engine-turn-hold.ts.
//!
//! One durable, session-scoped hold ledger, independent of the transcript leaf. Persisted as custom
//! entries `session-hold` (`{op: add|release}`), `session-notice` and `session-notices-consumed`
//! (the latter two owned by `engine_turn_hold_notices`). Session entries are untyped JSON.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::engine_turn_hold_notices::{
    EngineTurnHoldNotices, HeldItem, HeldItemReceipt, HeldMessage,
};
use crate::engine_turn_release::ReleasableHold;

pub const SESSION_HOLD_ENTRY_TYPE: &str = "session-hold";
pub const SESSION_NOTICE_ENTRY_TYPE: &str = "session-notice";
pub const SESSION_NOTICES_CONSUMED_ENTRY_TYPE: &str = "session-notices-consumed";
pub const PAUSE_NOT_SAVED_NOTICE: &str = "Pause could not be saved; it will not survive a restart.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldKind {
    Unattended,
    SessionHold,
    RunFence,
    InheritedHold,
}

impl HoldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unattended => "unattended",
            Self::SessionHold => "session_hold",
            Self::RunFence => "run_fence",
            Self::InheritedHold => "inherited_hold",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "unattended" => Some(Self::Unattended),
            "session_hold" => Some(Self::SessionHold),
            "run_fence" => Some(Self::RunFence),
            "inherited_hold" => Some(Self::InheritedHold),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldReleaseCause {
    Input,
    ResumeSession,
    RunResume,
    RunCancelled,
    NodeCancelled,
}

impl HoldReleaseCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::ResumeSession => "resume_session",
            Self::RunResume => "run_resume",
            Self::RunCancelled => "run_cancelled",
            Self::NodeCancelled => "node_cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionHold {
    pub kind: HoldKind,
    pub key: String,
    pub since: String,
    pub data: Option<Value>,
    pub source_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionHoldEvent {
    SessionNotice {
        notice_ids: Vec<String>,
        notice_count: usize,
        disposition: &'static str,
    },
    SessionPaused {
        pause_seq: u64,
        kind: HoldKind,
        at: String,
        notice_count: usize,
        persisted: bool,
    },
    SessionResumed {
        pause_seq: u64,
        cause: HoldReleaseCause,
        at: String,
    },
}

pub type HoldAppend = Arc<dyn Fn(&str, Value) -> Result<(), String> + Send + Sync>;
pub type HoldNotify = Arc<dyn Fn(&str) + Send + Sync>;
pub type HoldEmit = Arc<dyn Fn(SessionHoldEvent) + Send + Sync>;

/// Scan only ledger entries without materializing a compacted session's transcript
/// (`engineTurnHoldHistory` reducer).
#[derive(Debug, Clone, Default)]
pub struct EngineTurnHoldHistory {
    pub entries: Vec<Value>,
}

impl EngineTurnHoldHistory {
    pub fn add(&mut self, entry: &Value) {
        let ledger = entry.get("type").and_then(Value::as_str) == Some("custom")
            && matches!(
                entry.get("customType").and_then(Value::as_str),
                Some(
                    SESSION_HOLD_ENTRY_TYPE
                        | SESSION_NOTICE_ENTRY_TYPE
                        | SESSION_NOTICES_CONSUMED_ENTRY_TYPE
                )
            );
        if ledger {
            self.entries.push(entry.clone());
        }
    }

    pub fn from_entries<'a>(entries: impl IntoIterator<Item = &'a Value>) -> Self {
        let mut history = Self::default();
        for entry in entries {
            history.add(entry);
        }
        history
    }
}

/// The `engineTurnHoldHistory` reducer (senpi#2537), for `SessionManager::reduce_history`.
pub static ENGINE_TURN_HOLD_HISTORY: crate::session_history_reduce::HistoryReducer<
    EngineTurnHoldHistory,
> = crate::session_history_reduce::HistoryReducer {
    initial: EngineTurnHoldHistory::default,
    add: |history, entry| history.add(entry),
};

/// A hold shared between a session and an in-flight release: every call locks only for its own duration, so
/// the release flow can await between calls without keeping the hold locked.
pub struct SharedEngineTurnHold(pub Arc<std::sync::Mutex<EngineTurnHold>>);

impl SharedEngineTurnHold {
    fn with<T>(&self, run: impl FnOnce(&mut EngineTurnHold) -> T) -> T {
        run(&mut self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

impl ReleasableHold for SharedEngineTurnHold {
    fn held(&self) -> bool {
        self.with(|hold| hold.held())
    }
    fn releasing(&self) -> bool {
        self.with(|hold| ReleasableHold::releasing(hold))
    }
    fn generation(&self) -> u64 {
        self.with(|hold| ReleasableHold::generation(hold))
    }
    fn release_all_resume_session(&mut self) -> BTreeMap<String, usize> {
        self.with(ReleasableHold::release_all_resume_session)
    }
    fn messages(&self) -> Vec<HeldMessage> {
        self.with(|hold| EngineTurnHold::messages(hold))
    }
    fn consume(&mut self, source: &str, notice_ids: &[String]) -> bool {
        self.with(|hold| ReleasableHold::consume(hold, source, notice_ids))
    }
    fn reject_input(&mut self) {
        self.with(ReleasableHold::reject_input)
    }
    fn finish(&mut self) -> BTreeMap<String, usize> {
        self.with(ReleasableHold::finish)
    }
}

pub fn engine_wake_source(custom_type: &str) -> String {
    if custom_type == "goal-continuation" {
        "goal".to_owned()
    } else if custom_type.starts_with("senpi-monitor:") {
        "monitor".to_owned()
    } else if custom_type.starts_with("senpi-codemode:") {
        "codemode".to_owned()
    } else {
        custom_type.to_owned()
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub struct EngineTurnHold {
    records: Vec<(String, SessionHold)>,
    notices: EngineTurnHoldNotices,
    pause_seq: u64,
    pause_open: bool,
    released: Vec<SessionHold>,
    release_barrier: bool,
    goal_deferred: bool,
    generation: u64,
    attended: bool,
    append: HoldAppend,
    notify: HoldNotify,
    emit: HoldEmit,
}

impl EngineTurnHold {
    pub fn new(
        entries: &[Value],
        unattended: bool,
        append: HoldAppend,
        notify: HoldNotify,
        emit: HoldEmit,
    ) -> Self {
        let notice_append = Arc::clone(&append);
        let notices = EngineTurnHoldNotices::new(
            entries,
            Box::new(move |kind, data| notice_append(kind, data)),
            Box::new(write_failed),
        );
        let mut this = Self {
            records: Vec::new(),
            notices,
            pause_seq: 0,
            pause_open: false,
            released: Vec::new(),
            release_barrier: false,
            goal_deferred: false,
            generation: 0,
            attended: !unattended,
            append,
            notify,
            emit,
        };
        for entry in entries {
            if entry.get("type").and_then(Value::as_str) != Some("custom")
                || entry.get("customType").and_then(Value::as_str) != Some(SESSION_HOLD_ENTRY_TYPE)
            {
                continue;
            }
            let data = entry.get("data").cloned().unwrap_or(Value::Null);
            match data.get("op").and_then(Value::as_str) {
                Some("add") => {
                    let (Some(key), Some(kind)) = (
                        data.get("key").and_then(Value::as_str),
                        data.get("kind")
                            .and_then(Value::as_str)
                            .and_then(HoldKind::parse),
                    ) else {
                        continue;
                    };
                    this.pause_seq = this
                        .pause_seq
                        .max(data.get("pauseSeq").and_then(Value::as_u64).unwrap_or(0));
                    this.remove_record(key);
                    this.records.push((
                        key.to_owned(),
                        SessionHold {
                            kind,
                            key: key.to_owned(),
                            since: data
                                .get("at")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            data: data.get("data").filter(|value| !value.is_null()).cloned(),
                            source_counts: BTreeMap::new(),
                        },
                    ));
                }
                Some("release") => {
                    for key in data
                        .get("keys")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        this.remove_record(key);
                    }
                }
                _ => {}
            }
        }
        this.pause_open = this.is_held();
        this
    }

    fn remove_record(&mut self, key: &str) {
        self.records.retain(|(existing, _)| existing != key);
    }

    pub fn is_held(&self) -> bool {
        self.records
            .iter()
            .any(|(_, hold)| hold.kind != HoldKind::RunFence)
    }

    pub fn fences(&self) -> Vec<SessionHold> {
        self.holds()
            .into_iter()
            .filter(|hold| hold.kind == HoldKind::RunFence)
            .collect()
    }

    pub fn current_pause_seq(&self) -> u64 {
        self.pause_seq
    }

    pub fn held(&self) -> bool {
        self.is_held()
    }

    pub fn awaiting_input(&self) -> bool {
        self.is_held() || self.release_barrier
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn releasing(&self) -> bool {
        self.release_barrier
    }

    pub fn invalidate(&mut self) {
        self.generation += 1;
    }

    pub fn attend(&mut self) {
        self.attended = true;
    }

    pub fn sources(&self) -> BTreeMap<String, usize> {
        self.notices.sources().into_iter().collect()
    }

    pub fn notice_count(&self) -> usize {
        self.notices.notice_count()
    }

    pub fn holds(&self) -> Vec<SessionHold> {
        let sources = self.sources();
        self.records
            .iter()
            .map(|(_, hold)| SessionHold {
                source_counts: sources.clone(),
                ..hold.clone()
            })
            .collect()
    }

    pub fn get_hold(&self, key: &str) -> Option<SessionHold> {
        let (_, hold) = self.records.iter().find(|(existing, _)| existing == key)?;
        Some(SessionHold {
            source_counts: self.sources(),
            ..hold.clone()
        })
    }

    pub fn start(&mut self) {
        if !self.attended {
            self.add_hold(HoldKind::Unattended, None, None);
        }
        if self.is_held() && self.notice_count() > 0 {
            self.show_notice();
        }
    }

    pub fn add_hold(&mut self, kind: HoldKind, key: Option<&str>, data: Option<Value>) {
        let key = key.unwrap_or(kind.as_str()).to_owned();
        if self.records.iter().any(|(existing, _)| *existing == key) {
            return;
        }
        let at = now_iso();
        self.pause_seq += 1;
        self.records.push((
            key.clone(),
            SessionHold {
                kind,
                key: key.clone(),
                since: at.clone(),
                data: data.clone(),
                source_counts: BTreeMap::new(),
            },
        ));
        let mut entry = json!({"op": "add", "kind": kind.as_str(), "key": key, "pauseSeq": self.pause_seq, "at": at});
        if let (Some(data), Some(object)) = (data, entry.as_object_mut()) {
            object.insert("data".into(), data);
        }
        let persisted = match (self.append)(SESSION_HOLD_ENTRY_TYPE, entry) {
            Ok(()) => true,
            Err(error) => {
                write_failed(&error);
                false
            }
        };
        if !self.pause_open && self.is_held() {
            self.pause_open = true;
            (self.emit)(SessionHoldEvent::SessionPaused {
                pause_seq: self.pause_seq,
                kind,
                at,
                notice_count: self.notice_count(),
                persisted,
            });
        }
        if !persisted {
            (self.notify)(PAUSE_NOT_SAVED_NOTICE);
        }
    }

    /// Append before removal: a failed append keeps every hold in place and reports the error.
    pub fn release(&mut self, keys: &[String], cause: HoldReleaseCause) -> Result<(), String> {
        let mut present: Vec<String> = Vec::new();
        for key in keys {
            if self.records.iter().any(|(existing, _)| existing == key) && !present.contains(key) {
                present.push(key.clone());
            }
        }
        if present.is_empty() {
            return Ok(());
        }
        let at = now_iso();
        (self.append)(
            SESSION_HOLD_ENTRY_TYPE,
            json!({
                "op": "release", "keys": present, "observedSeq": self.pause_seq, "cause": cause.as_str(), "at": at,
            }),
        )?;
        for key in &present {
            self.remove_record(key);
        }
        if self.pause_open && !self.is_held() {
            self.pause_open = false;
            (self.emit)(SessionHoldEvent::SessionResumed {
                pause_seq: self.pause_seq,
                cause,
                at,
            });
        }
        Ok(())
    }

    pub fn holds_source(&self, source: &str) -> bool {
        self.is_held()
            || (!self.attended && self.release_barrier)
            || (source == "goal" && self.goal_deferred)
    }

    pub fn note(&mut self, source: &str, message: Option<Value>) {
        let id = self
            .notices
            .note_with_stop(source, self.pause_seq, message, self.stopped());
        (self.emit)(SessionHoldEvent::SessionNotice {
            notice_ids: vec![id],
            notice_count: self.notice_count(),
            disposition: "notice_persisted",
        });
        if !self.attended {
            self.show_notice();
        }
    }

    pub fn note_items(
        &mut self,
        items: &[HeldItem],
        append: impl FnOnce(Value) -> Result<String, String>,
    ) -> Result<Vec<HeldItemReceipt>, String> {
        let stopped = self.stopped();
        let receipts = self
            .notices
            .note_items(self.pause_seq, items, stopped, append)?;
        for receipt in &receipts {
            (self.emit)(SessionHoldEvent::SessionNotice {
                notice_ids: vec![receipt.notice_id.clone()],
                notice_count: self.notice_count(),
                disposition: "notice_persisted",
            });
        }
        if !self.attended {
            self.show_notice();
        }
        Ok(receipts)
    }

    pub fn messages(&self) -> Vec<HeldMessage> {
        self.notices.messages()
    }

    pub fn release_all(
        &mut self,
        cause: HoldReleaseCause,
    ) -> Result<BTreeMap<String, usize>, String> {
        let sources = self.sources();
        if self.release_barrier || !self.is_held() {
            return Ok(BTreeMap::new());
        }
        self.generation += 1;
        self.released = self.holds();
        self.release_barrier = true;
        self.goal_deferred = sources.get("goal").copied().unwrap_or(0) > 0;
        let keys: Vec<String> = self
            .records
            .iter()
            .filter(|(_, hold)| hold.kind != HoldKind::InheritedHold)
            .map(|(key, _)| key.clone())
            .collect();
        self.release(&keys, cause)?;
        Ok(sources)
    }

    /// Undo a release whose input never reached admission: lower the barrier and keep every held wake held.
    pub fn abandon_release(&mut self) {
        self.release_barrier = false;
        self.goal_deferred = false;
        self.rehold_released();
    }

    pub fn reject_input(&mut self) {
        self.release_barrier = false;
        self.goal_deferred = false;
        if !self.attended {
            self.rehold_released();
        }
    }

    fn rehold_released(&mut self) {
        let released = std::mem::take(&mut self.released);
        for SessionHold {
            kind, key, data, ..
        } in released
        {
            self.add_hold(kind, Some(&key), data);
        }
    }

    pub fn consume(&mut self, source: &str, delivered_ids: Option<&[String]>) -> bool {
        let ids: Vec<String> = self
            .notices
            .sources()
            .iter()
            .find(|(s, _)| s == source)
            .map(|_| {
                self.notices
                    .notice_ids(source)
                    .into_iter()
                    .filter(|id| delivered_ids.is_none_or(|ids| ids.contains(id)))
                    .collect()
            })
            .unwrap_or_default();
        if !self.notices.consume(source, self.pause_seq, delivered_ids) {
            return false;
        }
        (self.emit)(SessionHoldEvent::SessionNotice {
            notice_ids: ids,
            notice_count: self.notice_count(),
            disposition: "model_consumed",
        });
        true
    }

    pub fn finish(&mut self) -> BTreeMap<String, usize> {
        let sources = self.sources();
        self.notices.consume_notices(self.pause_seq);
        // Wakes arriving after the delivery snapshot still belong to the ledger: re-arm the hold
        // rather than draining an unbounded stream in this release.
        if !self.messages().is_empty() {
            self.reject_input();
            return sources;
        }
        self.consume("goal", None);
        self.goal_deferred = false;
        self.release_barrier = false;
        self.released.clear();
        sources
    }

    fn stopped(&self) -> bool {
        self.records
            .iter()
            .any(|(_, hold)| hold.kind == HoldKind::SessionHold)
    }

    fn show_notice(&self) {
        (self.notify)(&if self.stopped() {
            EngineTurnHoldNotices::stopped_notice_text(self.notice_count())
        } else {
            self.notices.notice_text()
        });
    }
}

impl ReleasableHold for EngineTurnHold {
    fn held(&self) -> bool {
        self.is_held()
    }
    fn releasing(&self) -> bool {
        self.release_barrier
    }
    fn generation(&self) -> u64 {
        self.generation
    }
    fn release_all_resume_session(&mut self) -> BTreeMap<String, usize> {
        self.release_all(HoldReleaseCause::ResumeSession)
            .unwrap_or_else(|error| {
                write_failed(&error);
                BTreeMap::new()
            })
    }
    fn messages(&self) -> Vec<HeldMessage> {
        EngineTurnHold::messages(self)
    }
    fn consume(&mut self, source: &str, notice_ids: &[String]) -> bool {
        EngineTurnHold::consume(self, source, Some(notice_ids))
    }
    fn reject_input(&mut self) {
        EngineTurnHold::reject_input(self)
    }
    fn finish(&mut self) -> BTreeMap<String, usize> {
        EngineTurnHold::finish(self)
    }
}

fn write_failed(error: &str) {
    crate::output_guard::maho_write_stderr(&format!("session_hold_write_failed {error}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Log = Arc<Mutex<Vec<(String, Value)>>>;

    type Notices = Arc<Mutex<Vec<String>>>;
    type HoldEvents = Arc<Mutex<Vec<SessionHoldEvent>>>;

    fn harness(
        entries: &[Value],
        unattended: bool,
        fail: bool,
    ) -> (EngineTurnHold, Log, Notices, HoldEvents) {
        let log: Log = Arc::default();
        let notices: Arc<Mutex<Vec<String>>> = Arc::default();
        let events: Arc<Mutex<Vec<SessionHoldEvent>>> = Arc::default();
        let sink = Arc::clone(&log);
        let append: HoldAppend = Arc::new(move |kind, data| {
            if fail {
                return Err("disk".to_owned());
            }
            if let Ok(mut log) = sink.lock() {
                log.push((kind.to_owned(), data));
            }
            Ok(())
        });
        let note_sink = Arc::clone(&notices);
        let notify: HoldNotify = Arc::new(move |message| {
            if let Ok(mut all) = note_sink.lock() {
                all.push(message.to_owned());
            }
        });
        let event_sink = Arc::clone(&events);
        let emit: HoldEmit = Arc::new(move |event| {
            if let Ok(mut all) = event_sink.lock() {
                all.push(event);
            }
        });
        (
            EngineTurnHold::new(entries, unattended, append, notify, emit),
            log,
            notices,
            events,
        )
    }

    fn ledger(log: &Log) -> Vec<Value> {
        log.lock()
            .map(|log| {
                log.iter()
                    .map(|(kind, data)| json!({"type": "custom", "customType": kind, "data": data}))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn fences_do_not_pause_and_inherited_holds_survive_human_resume() {
        let (mut hold, _, _, events) = harness(&[], false, false);
        hold.add_hold(HoldKind::RunFence, Some("run"), None);
        assert!(!hold.held());
        assert_eq!(hold.fences().len(), 1);
        assert!(events.lock().expect("events").is_empty());
        hold.add_hold(HoldKind::InheritedHold, Some("parent"), None);
        hold.add_hold(HoldKind::SessionHold, None, None);
        hold.release_all(HoldReleaseCause::Input).expect("release");
        assert!(hold.held());
        hold.release(&["parent".into()], HoldReleaseCause::RunResume)
            .expect("parent resumes");
        assert!(!hold.held());
        assert_eq!(events.lock().expect("events").len(), 2);
    }

    #[test]
    fn wake_sources_map_prefixes() {
        assert_eq!(engine_wake_source("goal-continuation"), "goal");
        assert_eq!(engine_wake_source("senpi-monitor:x"), "monitor");
        assert_eq!(engine_wake_source("senpi-codemode:y"), "codemode");
        assert_eq!(engine_wake_source("other"), "other");
    }

    #[test]
    fn unattended_start_holds_persists_and_replays() {
        let (mut hold, log, _, events) = harness(&[], true, false);
        hold.start();
        assert!(hold.held());
        assert!(matches!(
            events.lock().map(|e| e[0].clone()),
            Ok(SessionHoldEvent::SessionPaused {
                persisted: true,
                ..
            })
        ));
        let entries = ledger(&log);
        let (replayed, ..) = harness(&entries, false, false);
        assert!(replayed.held());
        assert_eq!(replayed.holds()[0].key, "unattended");
    }

    #[test]
    fn unpersisted_pause_notifies() {
        let (mut hold, _, notices, _) = harness(&[], true, true);
        hold.start();
        assert!(hold.held());
        assert_eq!(
            notices.lock().map(|n| n.clone()).unwrap_or_default(),
            vec![PAUSE_NOT_SAVED_NOTICE.to_owned()]
        );
    }

    #[test]
    fn release_all_then_abandon_restores_the_hold() {
        let (mut hold, _, _, events) = harness(&[], true, false);
        hold.start();
        hold.note("goal", None);
        let generation = hold.generation();
        let sources = hold.release_all(HoldReleaseCause::Input);
        assert_eq!(sources.map(|s| s.get("goal").copied()), Ok(Some(1)));
        assert!(
            !hold.held()
                && hold.releasing()
                && hold.holds_source("goal")
                && hold.generation() == generation + 1
        );
        assert!(matches!(
            events.lock().map(|e| e.last().cloned()),
            Ok(Some(SessionHoldEvent::SessionResumed {
                cause: HoldReleaseCause::Input,
                ..
            }))
        ));
        hold.abandon_release();
        assert!(hold.held() && !hold.releasing());
    }

    #[test]
    fn finish_clears_barrier_and_consumes_goal() {
        let (mut hold, _, _, _) = harness(&[], true, false);
        hold.start();
        hold.note("goal", None);
        assert!(hold.release_all(HoldReleaseCause::Input).is_ok());
        let remaining = hold.finish();
        assert_eq!(remaining.get("goal"), Some(&1));
        assert!(!hold.releasing() && hold.notice_count() == 0 && !hold.holds_source("goal"));
    }

    #[test]
    fn history_reducer_keeps_only_ledger_entries() {
        let entries = vec![
            json!({"type": "custom", "customType": "session-hold", "data": {}}),
            json!({"type": "custom", "customType": "other"}),
            json!({"type": "message"}),
            json!({"type": "custom", "customType": "session-notice"}),
        ];
        assert_eq!(
            EngineTurnHoldHistory::from_entries(&entries).entries.len(),
            2
        );
    }
}
