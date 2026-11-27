//! Port of `background-age.ts` (senpi#6201 / #6212):
//! Age notices of still-running background sessions: one at 10 minutes, then one every 30 minutes
//! (10, 40, 70, ...). One schedule per session bundle serves every session with ONE timer armed
//! for the earliest due notice, cleared when the last session leaves and on dispose. A late timer
//! or a session registered after it started reports at most once for the milestones it missed.

use maho_ext_builtin_loose::monitor_state_event::{
    TerminalBackgroundAgeEvent, TerminalBackgroundAgeKind, TerminalBackgroundAgeStop,
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MINUTE_MS: f64 = 60_000.0;
pub const BACKGROUND_AGE_FIRST_MS: f64 = 10.0 * MINUTE_MS;
pub const BACKGROUND_AGE_REPEAT_MS: f64 = 30.0 * MINUTE_MS;
const MAX_TIMER_MS: f64 = 2_147_483_647.0;

fn now_epoch_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

/// The first age milestone strictly after `age_ms`.
pub fn next_age_milestone_ms(age_ms: f64) -> f64 {
    if !age_ms.is_finite() || age_ms < BACKGROUND_AGE_FIRST_MS {
        return BACKGROUND_AGE_FIRST_MS;
    }
    let repeats = ((age_ms - BACKGROUND_AGE_FIRST_MS) / BACKGROUND_AGE_REPEAT_MS).floor() + 1.0;
    BACKGROUND_AGE_FIRST_MS + repeats * BACKGROUND_AGE_REPEAT_MS
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundAgeSubject {
    pub id: String,
    pub description: String,
    pub command: Option<String>,
    pub started_at_ms: f64,
    pub auto_reap_minutes: Option<f64>,
}

pub fn background_age_event(
    kind: TerminalBackgroundAgeKind,
    subject: &BackgroundAgeSubject,
    now_ms: f64,
) -> TerminalBackgroundAgeEvent {
    let age_ms = (now_ms - subject.started_at_ms).max(0.0);
    TerminalBackgroundAgeEvent {
        kind,
        id: subject.id.clone(),
        description: subject.description.clone(),
        command: subject.command.clone(),
        started_at_ms: subject.started_at_ms,
        age_ms,
        age_minutes: (age_ms / MINUTE_MS).floor(),
        auto_reap_minutes: subject.auto_reap_minutes,
        reason: None,
        stop: TerminalBackgroundAgeStop {
            bash_id: subject.id.clone(),
        },
    }
}

pub fn terminal_background_age_to_json(event: &TerminalBackgroundAgeEvent) -> Value {
    let kind = match event.kind {
        TerminalBackgroundAgeKind::Running => "running",
        TerminalBackgroundAgeKind::Reaped => "reaped",
        TerminalBackgroundAgeKind::ReapFailed => "reap_failed",
    };
    let mut map = Map::new();
    map.insert("kind".to_owned(), json!(kind));
    map.insert("id".to_owned(), json!(event.id));
    map.insert("description".to_owned(), json!(event.description));
    if let Some(command) = &event.command {
        map.insert("command".to_owned(), json!(command));
    }
    map.insert("startedAtMs".to_owned(), json!(event.started_at_ms));
    map.insert("ageMs".to_owned(), json!(event.age_ms));
    map.insert("ageMinutes".to_owned(), json!(event.age_minutes));
    map.insert(
        "autoReapMinutes".to_owned(),
        match event.auto_reap_minutes {
            Some(minutes) => json!(minutes),
            None => Value::Null,
        },
    );
    if let Some(reason) = &event.reason {
        map.insert("reason".to_owned(), json!(reason));
    }
    map.insert(
        "stop".to_owned(),
        json!({
            "tool": "kill_bash",
            "args": { "bash_id": event.stop.bash_id }
        }),
    );
    Value::Object(map)
}

#[derive(Clone, Debug)]
struct Entry {
    subject: BackgroundAgeSubject,
    due_at_ms: f64,
}

type DueCallback = Arc<dyn Fn(TerminalBackgroundAgeEvent) + Send + Sync>;

struct ScheduleInner {
    entries: BTreeMap<String, Entry>,
    on_due: DueCallback,
    timer: Option<tokio::task::JoinHandle<()>>,
    disposed: bool,
}

impl ScheduleInner {
    fn clear_timer(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
    }

    fn earliest_due_ms(&self) -> Option<f64> {
        self.entries
            .values()
            .map(|entry| entry.due_at_ms)
            .min_by(f64::total_cmp)
    }

    fn collect_due(&mut self, now_ms: f64) -> Vec<TerminalBackgroundAgeEvent> {
        let mut due = Vec::new();
        for entry in self.entries.values_mut() {
            if entry.due_at_ms > now_ms {
                continue;
            }
            let age_ms = now_ms - entry.subject.started_at_ms;
            due.push(background_age_event(
                TerminalBackgroundAgeKind::Running,
                &entry.subject,
                now_ms,
            ));
            entry.due_at_ms = entry.subject.started_at_ms + next_age_milestone_ms(age_ms);
        }
        due
    }
}

/// One shared age-notification schedule per session bundle.
#[derive(Clone)]
pub struct BackgroundAgeSchedule {
    inner: Arc<Mutex<ScheduleInner>>,
    now_ms: Arc<dyn Fn() -> f64 + Send + Sync>,
}

impl BackgroundAgeSchedule {
    pub fn new(on_due: impl Fn(TerminalBackgroundAgeEvent) + Send + Sync + 'static) -> Self {
        Self::with_clock(on_due, now_epoch_ms)
    }

    pub fn with_clock(
        on_due: impl Fn(TerminalBackgroundAgeEvent) + Send + Sync + 'static,
        now_ms: impl Fn() -> f64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ScheduleInner {
                entries: BTreeMap::new(),
                on_due: Arc::new(on_due),
                timer: None,
                disposed: false,
            })),
            now_ms: Arc::new(now_ms),
        }
    }

    pub fn add(&self, subject: BackgroundAgeSubject) {
        let now = (self.now_ms)();
        self.add_at(subject, now);
    }

    pub fn add_at(&self, subject: BackgroundAgeSubject, now_ms: f64) {
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.disposed {
                return;
            }
            let due_at_ms =
                subject.started_at_ms + next_age_milestone_ms(now_ms - subject.started_at_ms);
            inner
                .entries
                .insert(subject.id.clone(), Entry { subject, due_at_ms });
        }
        self.arm(now_ms);
    }

    pub fn get(&self, id: &str) -> Option<BackgroundAgeSubject> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .get(id)
            .map(|entry| entry.subject.clone())
    }

    pub fn remove(&self, id: &str) -> Option<BackgroundAgeSubject> {
        let removed = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.entries.remove(id).map(|entry| entry.subject)
        };
        if removed.is_some() {
            let now = (self.now_ms)();
            self.arm(now);
        }
        removed
    }

    pub fn dispose(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.disposed = true;
        inner.entries.clear();
        inner.clear_timer();
    }

    pub fn fire_due(&self, now_ms: f64) {
        let (due, on_due) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.disposed {
                return;
            }
            inner.timer = None;
            let due = inner.collect_due(now_ms);
            (due, inner.on_due.clone())
        };
        self.arm(now_ms);
        for event in due {
            on_due(event);
        }
    }

    fn arm(&self, now_ms: f64) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.clear_timer();
        if inner.disposed {
            return;
        }
        let Some(earliest) = inner.earliest_due_ms() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let delay_ms = (earliest - now_ms).clamp(0.0, MAX_TIMER_MS);
        let weak = Arc::downgrade(&self.inner);
        let clock = self.now_ms.clone();
        inner.timer = Some(handle.spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms.trunc() as u64)).await;
            let Some(strong) = weak.upgrade() else {
                return;
            };
            let schedule = BackgroundAgeSchedule {
                inner: strong,
                now_ms: clock.clone(),
            };
            let now = clock();
            schedule.fire_due(now);
        }));
    }
}

impl Drop for ScheduleInner {
    fn drop(&mut self) {
        self.clear_timer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_builtin_loose::monitor_state_event::is_terminal_background_age_event;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn milestones_advance_at_10_40_70_minutes_without_bursting_missed_windows() {
        assert_eq!(next_age_milestone_ms(0.0), 10.0 * MINUTE_MS);
        assert_eq!(next_age_milestone_ms(9.9 * MINUTE_MS), 10.0 * MINUTE_MS);
        assert_eq!(next_age_milestone_ms(10.0 * MINUTE_MS), 40.0 * MINUTE_MS);
        assert_eq!(next_age_milestone_ms(39.9 * MINUTE_MS), 40.0 * MINUTE_MS);
        assert_eq!(next_age_milestone_ms(40.0 * MINUTE_MS), 70.0 * MINUTE_MS);
        assert_eq!(next_age_milestone_ms(75.0 * MINUTE_MS), 100.0 * MINUTE_MS);
    }

    #[test]
    fn event_json_matches_loose_contract() {
        let subject = BackgroundAgeSubject {
            id: "bash_7".to_owned(),
            description: "soak".to_owned(),
            command: Some("./soak.sh".to_owned()),
            started_at_ms: 1_000.0,
            auto_reap_minutes: Some(45.0),
        };
        let event = background_age_event(
            TerminalBackgroundAgeKind::Running,
            &subject,
            1_000.0 + 10.0 * MINUTE_MS,
        );
        let value = terminal_background_age_to_json(&event);
        assert!(
            is_terminal_background_age_event(&value),
            "invalid payload: {value}"
        );
        assert_eq!(value["ageMinutes"], 10.0);
        assert_eq!(value["autoReapMinutes"], 45.0);
        assert_eq!(value["stop"]["tool"], "kill_bash");
        assert_eq!(value["stop"]["args"]["bash_id"], "bash_7");
    }

    #[tokio::test(start_paused = true)]
    async fn schedule_fires_at_10m_then_40m_and_remove_cancels_timer() {
        let seen = Arc::new(Mutex::new(Vec::<TerminalBackgroundAgeEvent>::new()));
        let sink = seen.clone();
        let clock = Arc::new(AtomicU64::new(0));
        let clock_read = clock.clone();
        let schedule = BackgroundAgeSchedule::with_clock(
            move |event| sink.lock().expect("seen").push(event),
            move || clock_read.load(Ordering::SeqCst) as f64,
        );
        schedule.add(BackgroundAgeSubject {
            id: "bash_1".to_owned(),
            description: "dev server".to_owned(),
            command: Some("npm run dev".to_owned()),
            started_at_ms: 0.0,
            auto_reap_minutes: None,
        });
        tokio::task::yield_now().await;
        assert!(seen.lock().expect("seen").is_empty());

        clock.store((10.0 * MINUTE_MS) as u64, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis((10.0 * MINUTE_MS) as u64)).await;
        tokio::task::yield_now().await;
        assert_eq!(seen.lock().expect("seen").len(), 1);
        assert_eq!(seen.lock().expect("seen")[0].age_minutes, 10.0);

        clock.store((40.0 * MINUTE_MS) as u64, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis((30.0 * MINUTE_MS) as u64)).await;
        tokio::task::yield_now().await;
        assert_eq!(seen.lock().expect("seen").len(), 2);
        assert_eq!(seen.lock().expect("seen")[1].age_minutes, 40.0);

        assert!(schedule.remove("bash_1").is_some());
        clock.store((70.0 * MINUTE_MS) as u64, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis((30.0 * MINUTE_MS) as u64)).await;
        tokio::task::yield_now().await;
        assert_eq!(seen.lock().expect("seen").len(), 2);
    }

    #[test]
    fn late_session_fires_once_for_missed_milestones() {
        let seen = Arc::new(Mutex::new(Vec::<f64>::new()));
        let sink = seen.clone();
        let schedule = BackgroundAgeSchedule::with_clock(
            move |event| sink.lock().expect("seen").push(event.age_minutes),
            || 0.0,
        );
        // Registered 50 minutes after start: next due is 70m, then fire_due at 105m fires once.
        schedule.add_at(
            BackgroundAgeSubject {
                id: "bash_2".to_owned(),
                description: "late".to_owned(),
                command: None,
                started_at_ms: 0.0,
                auto_reap_minutes: None,
            },
            50.0 * MINUTE_MS,
        );
        schedule.fire_due(69.0 * MINUTE_MS);
        assert!(seen.lock().expect("seen").is_empty());
        schedule.fire_due(105.0 * MINUTE_MS);
        assert_eq!(*seen.lock().expect("seen"), [105.0]);
    }
}
