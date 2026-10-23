use crate::{
    host_lifecycle::parse_idle_exit_ms,
    loop_blocked_time::LoopBlockedTime,
    session_attribution::SessionActivityRegistry,
};
use std::collections::HashMap;
pub const LOOP_LAG_WARN_MS_ENV: &str = "SENPI_RPC_LOOP_LAG_WARN_MS";
pub const LOOP_LAG_ERROR_MS_ENV: &str = "SENPI_RPC_LOOP_LAG_ERROR_MS";
pub const DEFAULT_LOOP_LAG_WARN_MS: f64 = 500.;
pub const DEFAULT_LOOP_LAG_ERROR_MS: f64 = 5000.;
pub const LOOP_LAG_TICK_MS: f64 = 200.;
/// Measurement interval once no session has done work for `LOOP_LAG_IDLE_AFTER_MS` (senpi#3738).
pub const LOOP_LAG_IDLE_TICK_MS: f64 = 1000.;
/// How long the host must stay without session activity before the tick slows down.
pub const LOOP_LAG_IDLE_AFTER_MS: f64 = 2000.;
pub const LOOP_LAG_WARN_INTERVAL_MS: f64 = 10000.;
const HEARTBEAT_THRESHOLD_FRACTION: f64 = 5.;
/// A beat counts as due this much before a full interval, so timer and clock rounding never skips one.
pub const HEARTBEAT_EARLY_TOLERANCE_MS: f64 = LOOP_LAG_TICK_MS / 2.;
/// Whether a heartbeat is due at `now` given the last one written (senpi `beat`'s due check).
pub fn heartbeat_due(last_heartbeat_at: Option<f64>, now: f64, heartbeat_interval_ms: f64) -> bool {
    last_heartbeat_at
        .is_none_or(|last| now - last >= heartbeat_interval_ms - HEARTBEAT_EARLY_TOLERANCE_MS)
}
pub struct LoopLagWatchdog {
    warn_ms: f64,
    error_ms: f64,
    expected_tick_at: Option<f64>,
    activity_mark: u64,
    last_warn_at: Option<f64>,
    interval_ms: f64,
    quiet_ms: f64,
    idle_interval_ms: f64,
}
#[derive(Debug, Default)]
pub struct LagSample {
    pub record: Option<serde_json::Value>,
    pub log: Option<String>,
}
fn heartbeat_interval_ms(error_ms: f64) -> f64 {
    LOOP_LAG_TICK_MS.max((error_ms / HEARTBEAT_THRESHOLD_FRACTION).floor())
}
use crate::session_attribution::describe_attribution as describe;
impl LoopLagWatchdog {
    pub async fn run_shared(
        &mut self,
        registry: &SessionActivityRegistry,
        blocked: &std::sync::Mutex<LoopBlockedTime>,
        now: impl FnMut() -> f64,
        publish: impl FnMut(LagSample),
        stopped: tokio::sync::watch::Receiver<bool>,
    ) {
        self.run_shared_with_wake(registry, blocked, now, publish, stopped, None)
            .await;
    }
    /// `run_shared` plus an activity-start signal: a `notify_one` restores the 200 ms tick at once
    /// (senpi `onSessionActivityStart`). Without one the slow tick is restored by the first tick that sees activity.
    pub async fn run_shared_with_wake(
        &mut self,
        registry: &SessionActivityRegistry,
        blocked: &std::sync::Mutex<LoopBlockedTime>,
        mut now: impl FnMut() -> f64,
        mut publish: impl FnMut(LagSample),
        mut stopped: tokio::sync::watch::Receiver<bool>,
        wake: Option<std::sync::Arc<tokio::sync::Notify>>,
    ) {
        self.start(now(), registry);
        // One timer across iterations: a stop signal that does not end the loop must not restart the interval.
        let mut sleep = Box::pin(tokio::time::sleep(std::time::Duration::from_millis(
            self.interval_ms as u64,
        )));
        loop {
            if *stopped.borrow() {
                break;
            }
            let woken = async {
                match &wake {
                    Some(wake) => wake.notified().await,
                    None => std::future::pending().await,
                }
            };
            let rearm = tokio::select! {
                changed=stopped.changed()=>{if changed.is_err(){break;}false},
                ()=woken=>{self.wake(now());true},
                ()=&mut sleep=>{
                    let sample=self.tick(now(),registry,&mut blocked.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
                    publish(sample);
                    true
                }
            };
            if rearm {
                sleep.as_mut().reset(
                    tokio::time::Instant::now()
                        + std::time::Duration::from_millis(self.interval_ms as u64),
                );
            }
        }
        self.stop();
    }
    pub async fn run(
        &mut self,
        registry: &SessionActivityRegistry,
        blocked: &mut LoopBlockedTime,
        mut now: impl FnMut() -> f64,
        mut publish: impl FnMut(LagSample),
    ) {
        self.start(now(), registry);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(LOOP_LAG_TICK_MS as u64)).await;
            publish(self.tick(now(), registry, blocked));
        }
    }
    pub fn new(env: &HashMap<String, String>) -> Self {
        Self {
            warn_ms: parse_idle_exit_ms(env.get(LOOP_LAG_WARN_MS_ENV).map(String::as_str))
                .unwrap_or(DEFAULT_LOOP_LAG_WARN_MS),
            error_ms: parse_idle_exit_ms(env.get(LOOP_LAG_ERROR_MS_ENV).map(String::as_str))
                .unwrap_or(DEFAULT_LOOP_LAG_ERROR_MS),
            expected_tick_at: None,
            activity_mark: 0,
            last_warn_at: None,
            interval_ms: LOOP_LAG_TICK_MS,
            quiet_ms: 0.,
            idle_interval_ms: LOOP_LAG_IDLE_TICK_MS.min(heartbeat_interval_ms(
                parse_idle_exit_ms(env.get(LOOP_LAG_ERROR_MS_ENV).map(String::as_str))
                    .unwrap_or(DEFAULT_LOOP_LAG_ERROR_MS),
            )),
        }
    }
    /// Interval until the next tick: 200 ms while active, slower once the host has been quiet.
    pub fn interval_ms(&self) -> f64 {
        self.interval_ms
    }
    /// Back to the 200 ms tick, from now: attributed work just started and may block the loop next.
    pub fn wake(&mut self, now: f64) {
        self.quiet_ms = 0.;
        if self.expected_tick_at.is_none() || self.interval_ms == LOOP_LAG_TICK_MS {
            return;
        }
        self.interval_ms = LOOP_LAG_TICK_MS;
        self.expected_tick_at = Some(now + LOOP_LAG_TICK_MS);
    }
    pub fn start(&mut self, now: f64, registry: &SessionActivityRegistry) {
        if self.expected_tick_at.is_none() {
            self.interval_ms = LOOP_LAG_TICK_MS;
            self.quiet_ms = 0.;
            self.expected_tick_at = Some(now + LOOP_LAG_TICK_MS);
            self.activity_mark = registry.mark();
        }
    }
    pub fn stop(&mut self) {
        self.expected_tick_at = None;
    }
    pub fn tick(
        &mut self,
        now: f64,
        registry: &SessionActivityRegistry,
        blocked: &mut LoopBlockedTime,
    ) -> LagSample {
        let expected = self.expected_tick_at.unwrap_or(now);
        let previous = self.activity_mark;
        self.activity_mark = registry.mark();
        let attribution = registry.since(previous);
        let quiet = self.activity_mark == previous && attribution.is_none();
        self.quiet_ms = if quiet {
            self.quiet_ms + self.interval_ms
        } else {
            0.
        };
        self.interval_ms = if self.quiet_ms >= LOOP_LAG_IDLE_AFTER_MS {
            self.idle_interval_ms
        } else {
            LOOP_LAG_TICK_MS
        };
        self.expected_tick_at = Some(now + self.interval_ms);
        let drift = (now - expected + 0.5).floor();
        blocked.record_loop_blocked_ms(drift);
        let mut sample = LagSample::default();
        if drift <= self.warn_ms {
            return sample;
        }
        if drift > self.error_ms {
            let mut record = serde_json::json!({"type":"host_stalled","driftMs":drift});
            if let Some(attribution) = &attribution {
                if let Some(session) = &attribution.session_id {
                    record["sessionId"] = session.clone().into();
                }
                if let Some(tool) = &attribution.tool {
                    record["tool"] = tool.clone().into();
                }
                if let Some(job) = attribution.job.as_ref().filter(|job| !job.is_empty()) {
                    record["job"] = job.clone().into();
                }
            }
            sample.record = Some(record);
        }
        if self
            .last_warn_at
            .is_some_and(|last| now - last < LOOP_LAG_WARN_INTERVAL_MS)
        {
            return sample;
        }
        self.last_warn_at = Some(now);
        sample.log = Some(format!(
            "senpi rpc host stall: event loop blocked {drift}ms ({})\n",
            describe(attribution.as_ref())
        ));
        sample
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_attribution::SessionAttribution;
    #[test]
    fn records_drift_and_attributes_errors_even_when_warning_throttled() {
        let registry = SessionActivityRegistry::default();
        let mut blocked = LoopBlockedTime::default();
        let mut watchdog = LoopLagWatchdog::new(&HashMap::new());
        watchdog.start(0., &registry);
        let _span = registry.open_span(
            SessionAttribution {
                session_id: Some("s".into()),
                tool: Some("bash".into()),
                job: None,
            },
            None,
        );
        let sample = watchdog.tick(6000., &registry, &mut blocked);
        assert_eq!(sample.record.unwrap()["sessionId"], "s");
        assert!(sample.log.is_some());
        let sample = watchdog.tick(12000., &registry, &mut blocked);
        assert!(sample.record.is_some());
        assert!(sample.log.is_none());
        assert_eq!(blocked.loop_blocked_mark(), 11600.);
    }
    #[test]
    fn quiet_host_slows_the_tick_and_wake_restores_it() {
        let registry = SessionActivityRegistry::default();
        let mut blocked = LoopBlockedTime::default();
        let mut watchdog = LoopLagWatchdog::new(&HashMap::new());
        watchdog.start(0., &registry);
        let mut at = 0.;
        for _ in 0..9 {
            at += watchdog.interval_ms();
            watchdog.tick(at, &registry, &mut blocked);
            assert_eq!(watchdog.interval_ms(), LOOP_LAG_TICK_MS);
        }
        at += watchdog.interval_ms();
        watchdog.tick(at, &registry, &mut blocked);
        assert_eq!(watchdog.interval_ms(), LOOP_LAG_IDLE_TICK_MS);
        watchdog.wake(at);
        assert_eq!(watchdog.interval_ms(), LOOP_LAG_TICK_MS);
        at += LOOP_LAG_TICK_MS;
        watchdog.tick(at, &registry, &mut blocked);
        assert_eq!(blocked.loop_blocked_mark(), 0.);
    }
    #[test]
    fn activity_keeps_the_fast_tick() {
        let registry = SessionActivityRegistry::default();
        let mut blocked = LoopBlockedTime::default();
        let mut watchdog = LoopLagWatchdog::new(&HashMap::new());
        watchdog.start(0., &registry);
        let mut at = 0.;
        for _ in 0..20 {
            let _span = registry.open_span(SessionAttribution::default(), None);
            at += watchdog.interval_ms();
            watchdog.tick(at, &registry, &mut blocked);
            assert_eq!(watchdog.interval_ms(), LOOP_LAG_TICK_MS);
        }
    }
    #[test]
    fn heartbeat_is_due_slightly_before_a_full_interval() {
        assert!(heartbeat_due(None, 0., 1000.));
        assert!(!heartbeat_due(Some(0.), 899., 1000.));
        assert!(heartbeat_due(Some(0.), 900., 1000.));
    }
    #[test]
    fn first_manual_tick_only_sets_baseline() {
        let registry = SessionActivityRegistry::default();
        let mut blocked = LoopBlockedTime::default();
        let mut watchdog = LoopLagWatchdog::new(&HashMap::new());
        assert!(watchdog.tick(99999., &registry, &mut blocked).log.is_none());
        assert_eq!(blocked.loop_blocked_mark(), 0.);
    }
}
