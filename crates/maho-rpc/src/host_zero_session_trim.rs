//! Zero-session trim (senpi `host-zero-session-trim.ts`): when a host that held sessions drops to zero
//! of them, run one collection and report what it returned as a `host_trimmed` record.
//!
//! Ticks only while a trim is owed (senpi#3747): the registry reports every open and close through
//! [`ZeroSessionTrimmer::observe`], so a host whose session count holds still has nothing to check and
//! no timer runs. The timer is one-shot ([`ZeroSessionTrimmer::next_tick_delay`] arms it), never an interval.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
pub const ZERO_SESSION_TRIM_TICK_MS: u64 = 1_000;
pub const ZERO_SESSION_TRIM_MIN_INTERVAL_MS: u64 = 60_000;
/// Below this footprint a session-count drop with sessions still open does not collect.
pub const DECLINE_TRIM_FLOOR_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// A busy-host trim waits at least this many times the previous collection's pause.
pub const DECLINE_TRIM_PAUSE_FACTOR: u64 = 100;
const BYTES_PER_MEGABYTE: f64 = 1024. * 1024.;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFootprint {
    pub bytes: u64,
    pub measure: String,
}
struct PendingTrim {
    before_bytes: u64,
    collected: bool,
    sessions: usize,
    pause_ms: u64,
}
pub struct ZeroSessionTrimmerOptions {
    /// Receives one `host_trimmed` record.
    pub emit: Box<dyn FnMut(serde_json::Value) + Send>,
    /// Live sessions, including ones opening or closing.
    pub sessions: Box<dyn FnMut() -> usize + Send>,
    /// Wall-clock milliseconds.
    pub now: Box<dyn FnMut() -> u64 + Send>,
    /// Monotonic milliseconds for timing a collection; a wall-clock jump must not stretch the next wait.
    pub monotonic_now: Box<dyn FnMut() -> u64 + Send>,
    pub read_footprint: Box<dyn FnMut() -> ProcessFootprint + Send>,
    /// Runs one full collection; `false` when the runtime exposes none.
    pub collect: Box<dyn FnMut() -> bool + Send>,
}
pub struct ZeroSessionTrimmer {
    options: ZeroSessionTrimmerOptions,
    held_sessions: bool,
    last_session_count: usize,
    /// A drop not yet answered: kept across ticks until the busy interval has elapsed and the footprint was checked.
    declined: bool,
    /// A drop to zero that landed inside the spacing: answered when it ends, unless sessions came back first.
    zero_pending: bool,
    last_trim_at: Option<u64>,
    /// A collection that ran on the previous tick; its record waits for the allocator to return the pages.
    pending: Option<PendingTrim>,
    last_pause_ms: u64,
    started: bool,
}
impl ZeroSessionTrimmer {
    pub fn new(options: ZeroSessionTrimmerOptions) -> Self {
        Self {
            options,
            held_sessions: false,
            last_session_count: 0,
            declined: false,
            zero_pending: false,
            last_trim_at: None,
            pending: None,
            last_pause_ms: 0,
            started: false,
        }
    }
    pub fn start(&mut self) {
        self.started = true;
    }
    pub fn stop(&mut self) {
        self.started = false;
    }
    /// The registry's own signal: any session that existed, however briefly, makes the next zero a transition.
    pub fn observe(&mut self, size: usize) {
        if size > 0 {
            self.held_sessions = true;
        }
        if size < self.last_session_count {
            self.declined = true;
        }
        self.last_session_count = size;
    }
    pub fn check(&mut self) {
        self.evaluate();
    }
    /// A collection waiting to be reported, a drop waiting for its spacing, or a zero not yet answered.
    pub fn owes_tick(&self) -> bool {
        self.pending.is_some()
            || self.declined
            || self.zero_pending
            || (self.held_sessions && self.last_session_count == 0)
    }
    /// The one-shot timer to arm now: `Some(tick)` only while started and a trim is owed.
    pub fn next_tick_delay(&self) -> Option<Duration> {
        (self.started && self.owes_tick()).then(|| Duration::from_millis(ZERO_SESSION_TRIM_TICK_MS))
    }
    fn evaluate(&mut self) {
        if let Some(pending) = self.pending.take() {
            self.report(pending);
        }
        let sessions = (self.options.sessions)();
        if sessions < self.last_session_count {
            self.declined = true;
        }
        self.last_session_count = sessions;
        if sessions > 0 {
            self.held_sessions = true;
            self.zero_pending = false;
            if !self.declined || !self.busy_interval_elapsed() {
                return;
            }
            self.declined = false;
            if (self.options.read_footprint)().bytes >= DECLINE_TRIM_FLOOR_BYTES {
                self.trim(sessions);
            }
            return;
        }
        self.declined = false;
        if !self.held_sessions && !self.zero_pending {
            return;
        }
        self.held_sessions = false;
        if !self.interval_elapsed(ZERO_SESSION_TRIM_MIN_INTERVAL_MS) {
            self.zero_pending = true;
            return;
        }
        self.zero_pending = false;
        self.trim(0);
    }
    fn interval_elapsed(&mut self, minimum_ms: u64) -> bool {
        let now = (self.options.now)();
        self.last_trim_at
            .is_none_or(|last| now.saturating_sub(last) >= minimum_ms)
    }
    fn busy_interval_elapsed(&mut self) -> bool {
        let minimum = ZERO_SESSION_TRIM_MIN_INTERVAL_MS
            .max(self.last_pause_ms.saturating_mul(DECLINE_TRIM_PAUSE_FACTOR));
        self.interval_elapsed(minimum)
    }
    fn trim(&mut self, sessions: usize) {
        let before_bytes = (self.options.read_footprint)().bytes;
        let started_at = (self.options.monotonic_now)();
        let collected = (self.options.collect)();
        self.last_pause_ms = (self.options.monotonic_now)().saturating_sub(started_at);
        self.last_trim_at = Some((self.options.now)());
        self.pending = Some(PendingTrim {
            before_bytes,
            collected,
            sessions,
            pause_ms: self.last_pause_ms,
        });
    }
    /// One tick after the collection: freed pages are decommitted lazily, so an immediate read shows none of it.
    fn report(&mut self, pending: PendingTrim) {
        let after = (self.options.read_footprint)();
        let megabytes = |bytes: u64| (bytes as f64 / BYTES_PER_MEGABYTE).round() as u64;
        (self.options.emit)(
            serde_json::json!({"type":"host_trimmed","footprintBeforeMb":megabytes(pending.before_bytes),"footprintAfterMb":megabytes(after.bytes),"measure":after.measure,"collected":pending.collected,"sessions":pending.sessions,"pauseMs":pending.pause_ms}),
        );
    }
}
/// Drives the one-shot timer: sleeps one tick only while a trim is owed, otherwise waits for `wake`
/// (notified on every `observe`) or `stopped`. Never polls an idle host.
pub async fn run_zero_session_trimmer(
    trimmer: Arc<Mutex<ZeroSessionTrimmer>>,
    wake: Arc<tokio::sync::Notify>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *stopped.borrow() {
            break;
        }
        let delay = trimmer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_tick_delay();
        match delay {
            Some(delay) => tokio::select! {
                changed=stopped.changed()=>{if changed.is_err(){break;}},
                ()=wake.notified()=>{},
                ()=tokio::time::sleep(delay)=>{trimmer.lock().unwrap_or_else(std::sync::PoisonError::into_inner).check();}
            },
            None => tokio::select! {
                changed=stopped.changed()=>{if changed.is_err(){break;}},
                ()=wake.notified()=>{},
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    struct Harness {
        trimmer: ZeroSessionTrimmer,
        sessions: Arc<AtomicUsize>,
        now: Arc<AtomicU64>,
        collected: Arc<AtomicUsize>,
        records: Arc<Mutex<Vec<serde_json::Value>>>,
        footprint: Arc<AtomicU64>,
    }
    fn harness() -> Harness {
        let sessions = Arc::new(AtomicUsize::new(0));
        let now = Arc::new(AtomicU64::new(1_000_000));
        let collected = Arc::new(AtomicUsize::new(0));
        let footprint = Arc::new(AtomicU64::new(3 * 1024 * 1024 * 1024));
        let records = Arc::new(Mutex::new(Vec::new()));
        let (s, n, m, c, f, r) = (
            sessions.clone(),
            now.clone(),
            now.clone(),
            collected.clone(),
            footprint.clone(),
            records.clone(),
        );
        let trimmer = ZeroSessionTrimmer::new(ZeroSessionTrimmerOptions {
            emit: Box::new(move |record| {
                r.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(record)
            }),
            sessions: Box::new(move || s.load(Ordering::SeqCst)),
            now: Box::new(move || n.load(Ordering::SeqCst)),
            monotonic_now: Box::new(move || m.load(Ordering::SeqCst)),
            read_footprint: Box::new(move || ProcessFootprint {
                bytes: f.load(Ordering::SeqCst),
                measure: "test".into(),
            }),
            collect: Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
                true
            }),
        });
        Harness {
            trimmer,
            sessions,
            now,
            collected,
            records,
            footprint,
        }
    }
    #[test]
    fn idle_host_that_never_held_a_session_owes_no_tick() {
        let mut h = harness();
        h.trimmer.start();
        h.trimmer.observe(0);
        assert!(h.trimmer.next_tick_delay().is_none());
    }
    #[test]
    fn timer_is_armed_only_while_started_and_owed() {
        let mut h = harness();
        h.trimmer.observe(1);
        assert!(h.trimmer.next_tick_delay().is_none());
        h.trimmer.start();
        assert!(h.trimmer.next_tick_delay().is_none());
        h.trimmer.observe(0);
        assert_eq!(
            h.trimmer.next_tick_delay(),
            Some(Duration::from_millis(ZERO_SESSION_TRIM_TICK_MS))
        );
        h.trimmer.stop();
        assert!(h.trimmer.next_tick_delay().is_none());
    }
    #[test]
    fn drop_to_zero_trims_then_reports_on_the_next_tick_and_goes_quiet() {
        let mut h = harness();
        h.trimmer.start();
        h.sessions.store(1, Ordering::SeqCst);
        h.trimmer.observe(1);
        h.sessions.store(0, Ordering::SeqCst);
        h.trimmer.observe(0);
        h.trimmer.check();
        assert_eq!(h.collected.load(Ordering::SeqCst), 1);
        assert!(h.trimmer.owes_tick());
        h.footprint.store(1024 * 1024 * 1024, Ordering::SeqCst);
        h.trimmer.check();
        let records = h
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["footprintBeforeMb"], 3072);
        assert_eq!(records[0]["footprintAfterMb"], 1024);
        assert_eq!(records[0]["sessions"], 0);
        drop(records);
        assert!(h.trimmer.next_tick_delay().is_none());
    }
    #[test]
    fn zero_inside_the_spacing_stays_owed_until_it_ends() {
        let mut h = harness();
        h.trimmer.start();
        h.trimmer.observe(1);
        h.trimmer.observe(0);
        h.trimmer.check();
        h.trimmer.check();
        h.now.fetch_add(1000, Ordering::SeqCst);
        h.trimmer.observe(1);
        h.sessions.store(1, Ordering::SeqCst);
        h.trimmer.observe(0);
        h.sessions.store(0, Ordering::SeqCst);
        h.trimmer.check();
        assert_eq!(h.collected.load(Ordering::SeqCst), 1);
        assert!(h.trimmer.owes_tick());
        h.now
            .fetch_add(ZERO_SESSION_TRIM_MIN_INTERVAL_MS, Ordering::SeqCst);
        h.trimmer.check();
        assert_eq!(h.collected.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn busy_drop_below_the_floor_never_collects() {
        let mut h = harness();
        h.trimmer.start();
        h.footprint.store(1024, Ordering::SeqCst);
        h.sessions.store(2, Ordering::SeqCst);
        h.trimmer.observe(3);
        h.trimmer.observe(2);
        h.trimmer.check();
        assert_eq!(h.collected.load(Ordering::SeqCst), 0);
        assert!(!h.trimmer.owes_tick());
    }
    #[tokio::test]
    async fn driver_runs_a_one_shot_check_after_a_wake_and_stops_on_signal() {
        let h = harness();
        let records = h.records.clone();
        let sessions = h.sessions.clone();
        let trimmer = Arc::new(Mutex::new(h.trimmer));
        trimmer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .start();
        let wake = Arc::new(tokio::sync::Notify::new());
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn(run_zero_session_trimmer(
            trimmer.clone(),
            wake.clone(),
            stopped,
        ));
        sessions.store(1, Ordering::SeqCst);
        {
            let mut guard = trimmer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.observe(1);
        }
        wake.notify_one();
        sessions.store(0, Ordering::SeqCst);
        {
            let mut guard = trimmer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.observe(0);
        }
        wake.notify_one();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if !records
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .ok();
        stop.send(true).ok();
        tokio::time::timeout(Duration::from_secs(5), driver)
            .await
            .ok();
        assert_eq!(
            records
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );
    }
}
