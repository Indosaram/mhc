//! senpi `modes/rpc/tool-child/job-supervisor.ts` (pin 26acd9353).
//!
//! Applies the job policy to one live tool child (senpi#3566, plan todo 6): each job's server backstop, the cancel
//! ladder (cancel -> 5 s -> TERM -> 2 s -> KILL) and control liveness (a ping every 5 s while a job is pending; three
//! missed pongs end the child). Timers are injected, so every rule is exact under a fake clock.
//!
//! The job's `AbortSignal` has no synchronous listener on `maho_ext_api::AbortSignal`, so `track` watches it with a
//! task on the current tokio runtime; the abort is seen on the runtime's next turn instead of in the aborting call.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use maho_ext_api::AbortSignal;

use crate::job_policy::{
    CANCEL_LADDER, CancelStep, Liveness, MISSED_PONGS_LIMIT, PING_INTERVAL_MS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildSignal {
    Sigterm,
    Sigkill,
}

impl ChildSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sigterm => "SIGTERM",
            Self::Sigkill => "SIGKILL",
        }
    }
}

pub trait SupervisedChild: Send + Sync {
    fn send_cancel(&self, job_id: &str);
    fn send_ping(&self, seq: u64);
    /// TERM ends the child gracefully (Windows: the job is terminated); KILL ends it at once.
    fn signal(&self, signal: ChildSignal);
}

/// Stops a timer set through `SupervisorTimers`.
pub type CancelTimer = Box<dyn FnOnce() + Send>;

pub trait SupervisorTimers: Send + Sync {
    fn set(&self, f: Box<dyn FnOnce() + Send>, ms: f64) -> CancelTimer;
}

/// The wall-clock timers, on the current tokio runtime.
pub struct RealTimers;

pub const REAL_TIMERS: RealTimers = RealTimers;

impl SupervisorTimers for RealTimers {
    fn set(&self, f: Box<dyn FnOnce() + Send>, ms: f64) -> CancelTimer {
        let handle = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(ms.max(0.0) / 1000.0)).await;
            f();
        });
        Box::new(move || handle.abort())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSupervisorError {
    pub code: String,
    /// `<code>: <message>`, as upstream's `Error.message`.
    pub message: String,
}

impl JobSupervisorError {
    pub fn new(code: impl Into<String>, message: impl AsRef<str>) -> Self {
        let code = code.into();
        let message = format!("{code}: {}", message.as_ref());
        Self { code, message }
    }
}

impl std::fmt::Display for JobSupervisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JobSupervisorError {}

pub type RejectJob = Arc<dyn Fn(JobSupervisorError) + Send + Sync>;

pub struct TrackOptions {
    pub reject: RejectJob,
    pub backstop_ms: Option<f64>,
    pub user_text: String,
    pub signal: Option<AbortSignal>,
}

struct TrackedJob {
    /// Tracking order (a JS `Map` iterates in insertion order).
    order: u64,
    reject: RejectJob,
    cancel_timers: Vec<CancelTimer>,
    ladder_timers: Vec<CancelTimer>,
    laddered: bool,
}

struct State {
    jobs: HashMap<String, TrackedJob>,
    liveness: Liveness,
    next_ping: u64,
    next_order: u64,
    stop_pings: Option<CancelTimer>,
    ended: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Inner {
    child: Arc<dyn SupervisedChild>,
    timers: Arc<dyn SupervisorTimers>,
    state: Mutex<State>,
}

pub struct JobSupervisor {
    inner: Arc<Inner>,
}

impl JobSupervisor {
    /// `timers` defaults to `REAL_TIMERS` when `None`.
    pub fn new(child: Arc<dyn SupervisedChild>, timers: Option<Arc<dyn SupervisorTimers>>) -> Self {
        Self {
            inner: Arc::new(Inner {
                child,
                timers: timers.unwrap_or_else(|| Arc::new(RealTimers)),
                state: Mutex::new(State {
                    jobs: HashMap::new(),
                    liveness: Liveness::new(),
                    next_ping: 0,
                    next_order: 0,
                    stop_pings: None,
                    ended: false,
                }),
            }),
        }
    }

    /// Starts supervising a job; `settle` must be called when its result, error or refusal arrives.
    pub fn track(&self, job_id: &str, options: TrackOptions) {
        let inner = &self.inner;
        let mut job = TrackedJob {
            order: 0,
            reject: options.reject.clone(),
            cancel_timers: Vec::new(),
            ladder_timers: Vec::new(),
            laddered: false,
        };
        if let Some(backstop_ms) = options.backstop_ms {
            let (target, reject, user_text) = (
                inner.clone(),
                options.reject.clone(),
                options.user_text.clone(),
            );
            let job_id = job_id.to_owned();
            job.cancel_timers.push(inner.timers.set(
                Box::new(move || {
                    reject(JobSupervisorError::new("tool_child_deadline", &user_text));
                    cancel(&target, &job_id);
                }),
                backstop_ms,
            ));
        }
        if let Some(signal) = options.signal {
            let (target, id) = (inner.clone(), job_id.to_owned());
            let watcher = tokio::spawn(async move {
                signal.cancelled().await;
                cancel(&target, &id);
            });
            job.cancel_timers.push(Box::new(move || watcher.abort()));
        }
        {
            let mut state = lock(&inner.state);
            job.order = state.next_order;
            state.next_order += 1;
            state.jobs.insert(job_id.to_owned(), job);
        }
        ensure_pinging(inner);
    }

    pub fn settle(&self, job_id: &str) {
        let (job, empty) = {
            let mut state = lock(&self.inner.state);
            let Some(job) = state.jobs.remove(job_id) else {
                return;
            };
            (job, state.jobs.is_empty())
        };
        for cancel in job.cancel_timers.into_iter().chain(job.ladder_timers) {
            cancel();
        }
        if empty {
            stop_pinging(&self.inner);
        }
    }

    /// The child acted on the cancel: the ladder stops; the job settles on its own, still under its backstop.
    pub fn cancel_acked(&self, job_id: &str) {
        let ladder: Vec<CancelTimer> = {
            let mut state = lock(&self.inner.state);
            match state.jobs.get_mut(job_id) {
                Some(job) => std::mem::take(&mut job.ladder_timers),
                None => return,
            }
        };
        for cancel in ladder {
            cancel();
        }
    }

    pub fn pong_received(&self) {
        lock(&self.inner.state).liveness.pong_received();
    }

    /// The child is gone: stop every timer; pending jobs are rejected by the caller's exit handling.
    pub fn end(&self) {
        end(&self.inner);
    }
}

fn end(inner: &Arc<Inner>) {
    lock(&inner.state).ended = true;
    stop_pinging(inner);
    let jobs: Vec<TrackedJob> = lock(&inner.state)
        .jobs
        .drain()
        .map(|(_, job)| job)
        .collect();
    for job in jobs {
        for cancel in job.cancel_timers.into_iter().chain(job.ladder_timers) {
            cancel();
        }
    }
}

fn cancel(inner: &Arc<Inner>, job_id: &str) {
    {
        let mut state = lock(&inner.state);
        let ended = state.ended;
        match state.jobs.get_mut(job_id) {
            Some(job) if !job.laddered && !ended => job.laddered = true,
            _ => return,
        }
    }
    inner.child.send_cancel(job_id);
    for rung in CANCEL_LADDER {
        let child = inner.child.clone();
        let step = rung.step;
        let timer = inner.timers.set(
            Box::new(move || {
                child.signal(match step {
                    CancelStep::Term => ChildSignal::Sigterm,
                    CancelStep::Kill => ChildSignal::Sigkill,
                });
            }),
            rung.at_ms,
        );
        match lock(&inner.state).jobs.get_mut(job_id) {
            Some(job) => job.ladder_timers.push(timer),
            None => timer(),
        }
    }
}

fn ensure_pinging(inner: &Arc<Inner>) {
    {
        let state = lock(&inner.state);
        if state.stop_pings.is_some() || state.ended {
            return;
        }
    }
    let stop = schedule_tick(inner);
    lock(&inner.state).stop_pings = Some(stop);
}

fn schedule_tick(inner: &Arc<Inner>) -> CancelTimer {
    let target = inner.clone();
    inner
        .timers
        .set(Box::new(move || tick(&target)), PING_INTERVAL_MS)
}

fn tick(inner: &Arc<Inner>) {
    let unresponsive = lock(&inner.state).liveness.unresponsive();
    if unresponsive {
        let failure = JobSupervisorError::new(
            "tool_child_unresponsive",
            format!("the tool child missed {MISSED_PONGS_LIMIT} pings"),
        );
        let mut rejects: Vec<(u64, RejectJob)> = lock(&inner.state)
            .jobs
            .values()
            .map(|job| (job.order, job.reject.clone()))
            .collect();
        rejects.sort_by_key(|(order, _)| *order);
        for (_, reject) in rejects {
            reject(failure.clone());
        }
        inner.child.signal(ChildSignal::Sigkill);
        end(inner);
        return;
    }
    let seq = {
        let mut state = lock(&inner.state);
        state.liveness.ping_sent();
        state.next_ping += 1;
        state.next_ping
    };
    inner.child.send_ping(seq);
    let stop = schedule_tick(inner);
    lock(&inner.state).stop_pings = Some(stop);
}

fn stop_pinging(inner: &Arc<Inner>) {
    let stop = lock(&inner.state).stop_pings.take();
    if let Some(stop) = stop {
        stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // senpi#3566 todo 6: job deadlines, the cancel ladder and control liveness against one tool child, under a fake
    // clock.

    struct Entry {
        at: f64,
        f: Option<Box<dyn FnOnce() + Send>>,
        live: bool,
    }

    #[derive(Default)]
    struct ClockState {
        now: f64,
        queue: Vec<Entry>,
    }

    #[derive(Clone, Default)]
    struct FakeClock {
        state: Arc<Mutex<ClockState>>,
    }

    impl SupervisorTimers for FakeClock {
        fn set(&self, f: Box<dyn FnOnce() + Send>, ms: f64) -> CancelTimer {
            let index = {
                let mut state = lock(&self.state);
                let at = state.now + ms;
                state.queue.push(Entry {
                    at,
                    f: Some(f),
                    live: true,
                });
                state.queue.len() - 1
            };
            let state = self.state.clone();
            Box::new(move || {
                if let Some(entry) = lock(&state).queue.get_mut(index) {
                    entry.live = false;
                }
            })
        }
    }

    impl FakeClock {
        fn now(&self) -> f64 {
            lock(&self.state).now
        }

        fn advance(&self, ms: f64) {
            let until = self.now() + ms;
            loop {
                let fired = {
                    let mut state = lock(&self.state);
                    let next = state
                        .queue
                        .iter()
                        .enumerate()
                        .filter(|(_, entry)| entry.live && entry.at <= until)
                        .min_by(|a, b| a.1.at.total_cmp(&b.1.at))
                        .map(|(index, _)| index);
                    next.and_then(|index| {
                        let at = state.queue[index].at;
                        state.now = at;
                        state.queue[index].live = false;
                        state.queue[index].f.take()
                    })
                };
                match fired {
                    Some(f) => f(),
                    None => break,
                }
            }
            lock(&self.state).now = until;
        }
    }

    struct RecordingChild {
        clock: FakeClock,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl RecordingChild {
        fn push(&self, event: String) {
            lock(&self.events).push(format!("{}:{event}", self.clock.now()));
        }
    }

    impl SupervisedChild for RecordingChild {
        fn send_cancel(&self, job_id: &str) {
            self.push(format!("cancel:{job_id}"));
        }
        fn send_ping(&self, seq: u64) {
            self.push(format!("ping:{seq}"));
        }
        fn signal(&self, signal: ChildSignal) {
            self.push(signal.as_str().to_owned());
        }
    }

    struct Rig {
        clock: FakeClock,
        events: Arc<Mutex<Vec<String>>>,
        supervisor: JobSupervisor,
        rejected: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Rig {
        fn new() -> Self {
            let clock = FakeClock::default();
            let events: Arc<Mutex<Vec<String>>> = Arc::default();
            let child = Arc::new(RecordingChild {
                clock: clock.clone(),
                events: events.clone(),
            });
            let supervisor = JobSupervisor::new(child, Some(Arc::new(clock.clone())));
            Self {
                clock,
                events,
                supervisor,
                rejected: Arc::default(),
            }
        }

        fn track(&self, job_id: &str, backstop_ms: Option<f64>, signal: Option<AbortSignal>) {
            let (rejected, id) = (self.rejected.clone(), job_id.to_owned());
            self.supervisor.track(
                job_id,
                TrackOptions {
                    reject: Arc::new(move |error| lock(&rejected).push((id.clone(), error.code))),
                    backstop_ms,
                    user_text: "The search took too long and was stopped.".to_owned(),
                    signal,
                },
            );
        }

        fn events(&self) -> Vec<String> {
            lock(&self.events).clone()
        }

        fn rejected(&self) -> Vec<(String, String)> {
            lock(&self.rejected).clone()
        }
    }

    /// Lets the abort watcher task run: a bounded number of yields, never a timed wait.
    async fn turns() {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn climbs_the_cancel_ladder_when_a_cancelled_job_does_not_settle_term_at_5_s_kill_at_7_s()
    {
        let rig = Rig::new();
        let abort = AbortSignal::default();
        rig.track("j1", None, Some(abort.clone()));
        abort.abort();
        turns().await;
        rig.clock.advance(7_000.0);
        let events: Vec<String> = rig
            .events()
            .into_iter()
            .filter(|event| !event.contains(":ping:"))
            .collect();
        assert_eq!(events, vec!["0:cancel:j1", "5000:SIGTERM", "7000:SIGKILL"]);
        rig.supervisor.end();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stops_the_ladder_when_the_child_acknowledges_the_cancel_the_backstop_still_applies() {
        let rig = Rig::new();
        let abort = AbortSignal::default();
        rig.track("j1", Some(30_000.0), Some(abort.clone()));
        abort.abort();
        turns().await;
        rig.supervisor.cancel_acked("j1");
        let mut elapsed = 0.0;
        while elapsed < 30_000.0 {
            rig.clock.advance(5_000.0);
            rig.supervisor.pong_received();
            elapsed += 5_000.0;
        }
        assert!(!rig.events().iter().any(|event| event.contains("SIG")));
        assert_eq!(
            rig.rejected(),
            vec![("j1".to_owned(), "tool_child_deadline".to_owned())]
        );
        rig.supervisor.end();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stops_the_ladder_when_the_job_settles_after_the_cancel() {
        let rig = Rig::new();
        let abort = AbortSignal::default();
        rig.track("j1", None, Some(abort.clone()));
        abort.abort();
        turns().await;
        rig.clock.advance(4_000.0);
        rig.supervisor.settle("j1");
        rig.clock.advance(10_000.0);
        assert_eq!(rig.events(), vec!["0:cancel:j1"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_a_job_at_its_backstop_with_the_kinds_user_text_then_cancels_it() {
        let rig = Rig::new();
        rig.track("j1", Some(120_000.0), None);
        let mut elapsed = 0.0;
        while elapsed < 120_000.0 {
            rig.clock.advance(5_000.0);
            rig.supervisor.pong_received();
            elapsed += 5_000.0;
        }
        assert_eq!(
            rig.rejected(),
            vec![("j1".to_owned(), "tool_child_deadline".to_owned())]
        );
        let cancels: Vec<String> = rig
            .events()
            .into_iter()
            .filter(|event| event.contains("cancel"))
            .collect();
        assert_eq!(cancels, vec!["120000:cancel:j1"]);
        rig.supervisor.end();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pings_every_5_s_while_a_job_is_pending_and_ends_the_child_after_three_missed_pongs() {
        let rig = Rig::new();
        rig.track("j1", None, None);
        rig.track("j2", None, None);
        rig.clock.advance(20_000.0);
        assert_eq!(
            rig.events(),
            vec![
                "5000:ping:1",
                "10000:ping:2",
                "15000:ping:3",
                "20000:SIGKILL"
            ]
        );
        assert_eq!(
            rig.rejected(),
            vec![
                ("j1".to_owned(), "tool_child_unresponsive".to_owned()),
                ("j2".to_owned(), "tool_child_unresponsive".to_owned()),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn keeps_a_child_that_answers_and_never_pings_while_nothing_is_pending() {
        let rig = Rig::new();
        rig.clock.advance(30_000.0);
        assert!(rig.events().is_empty());
        rig.track("j1", None, None);
        for _ in 0..10 {
            rig.clock.advance(5_000.0);
            rig.supervisor.pong_received();
        }
        assert!(!rig.events().iter().any(|event| event.contains("SIGKILL")));
        rig.supervisor.settle("j1");
        let pings_before = rig.events().len();
        rig.clock.advance(30_000.0);
        assert_eq!(rig.events().len(), pings_before);
    }
}
