//! The recorded-supervisor poll (senpi `host-supervisor-poll.ts`, senpi#3795): a free liveness check every tick and,
//! rarely, the recorded start compared on one clock per platform. It is the host's only binding when no event binding
//! exists, a backup cadence beside an inherited pipe (senpi#3738), and a single long backstop beside an event binding.
use std::time::Duration;

/// While an event binding (lifetime FIFO or reopened stdin) is armed, the identity check is one long backstop.
pub const HOST_WATCH_BACKSTOP_INTERVAL_MS: u64 = 30_000;
/// Poll cadence for the recorded-supervisor poll when it is the only binding.
pub const HOST_WATCH_PPID_INTERVAL_MS: u64 = 250;
/// Poll cadence while the inherited pipe is armed (senpi#3738): pipe EOF is the primary signal.
pub const HOST_WATCH_PPID_BACKUP_INTERVAL_MS: u64 = 5_000;
/// How often the recorded supervisor's start is compared on Linux (`/proc` start ticks, nothing spawned).
pub const HOST_WATCH_IDENTITY_INTERVAL_MS: u64 = 5_000;
/// Liveness cadence when the recorded supervisor identity is the host's ONLY binding (senpi#3816 review).
pub const HOST_WATCH_IDENTITY_ONLY_INTERVAL_MS: u64 = 1_000;
/// How often the recorded start is compared off Linux, where reading it may spawn a probe.
pub const HOST_WATCH_SPAWNING_IDENTITY_INTERVAL_MS: u64 = 60_000;

pub use crate::process_start_identity::{RecordedStart, StartComparison, compare_recorded_start};

/// The cadence when the poll is the host's only binding: identity-only (1 s) with a recorded start, else 250 ms.
pub fn sole_interval_ms(recorded: &RecordedStart) -> u64 {
    if recorded.ticks.is_some() || recorded.time.is_some() {
        HOST_WATCH_IDENTITY_ONLY_INTERVAL_MS
    } else {
        HOST_WATCH_PPID_INTERVAL_MS
    }
}

/// Liveness ticks between two start comparisons at `interval_ms`.
pub fn identity_every_ticks(linux: bool, interval_ms: u64) -> u64 {
    let every = if linux {
        HOST_WATCH_IDENTITY_INTERVAL_MS
    } else {
        HOST_WATCH_SPAWNING_IDENTITY_INTERVAL_MS
    };
    ((every as f64 / interval_ms.max(1) as f64).round() as u64).max(1)
}

/// What one poll needs from the OS, injectable for tests.
pub trait SupervisorProbe: Send {
    fn alive(&mut self, pid: u32) -> bool;
    fn start(&mut self, pid: u32) -> Option<String>;
}

/// The real probe: `kill(pid, 0)` and this crate's start-identity reader (ticks on Linux).
pub struct OsSupervisorProbe;
impl SupervisorProbe for OsSupervisorProbe {
    fn alive(&mut self, pid: u32) -> bool {
        crate::host_reservations::process_is_live(pid)
    }
    fn start(&mut self, pid: u32) -> Option<String> {
        if cfg!(target_os = "linux") {
            crate::process_start_identity::read_process_start_ticks(pid)
        } else {
            crate::host_reservations::read_process_start_time(pid)
        }
    }
}

/// Poll only the recorded supervisor, never the parent pid (a wrapper may be the parent). `interval` carries the
/// armed cadence: `None` keeps the poll disarmed, and every change re-arms it with a fresh tick count. Resolves with
/// the reason once the supervisor is gone; never resolves for a binding to this process itself.
pub async fn watch_supervisor_poll(
    supervisor_pid: u32,
    recorded: RecordedStart,
    mut interval: tokio::sync::watch::Receiver<Option<u64>>,
    mut probe: impl SupervisorProbe,
) -> String {
    // Tests and embedders may bind the watchdog to this process itself: a live binding, not supervisor loss.
    if supervisor_pid == std::process::id() {
        return std::future::pending().await;
    }
    let linux = cfg!(target_os = "linux");
    let comparable = if linux {
        recorded.ticks.is_some()
    } else {
        recorded.time.is_some()
    };
    loop {
        let Some(interval_ms) = *interval.borrow_and_update() else {
            if interval.changed().await.is_err() {
                return std::future::pending().await;
            }
            continue;
        };
        let period = Duration::from_millis(interval_ms);
        let every = identity_every_ticks(linux, interval_ms);
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ticks = 0u64;
        loop {
            tokio::select! {
                changed = interval.changed() => {
                    if changed.is_err() { return std::future::pending().await; }
                    break;
                }
                _ = timer.tick() => {
                    if !probe.alive(supervisor_pid) {
                        return format!("supervisor pid {supervisor_pid} is gone");
                    }
                    ticks += 1;
                    if !comparable || ticks % every != 0 {
                        continue;
                    }
                    let observed = probe.start(supervisor_pid);
                    // Permission and probe failures are unknown, never death.
                    if compare_recorded_start(linux, &recorded, observed.as_deref())
                        == StartComparison::Changed
                    {
                        return format!("supervisor pid {supervisor_pid} start time changed");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scripted {
        alive: bool,
        start: Option<String>,
    }
    impl SupervisorProbe for Scripted {
        fn alive(&mut self, _pid: u32) -> bool {
            self.alive
        }
        fn start(&mut self, _pid: u32) -> Option<String> {
            self.start.clone()
        }
    }
    fn recorded(value: &str) -> RecordedStart {
        RecordedStart {
            ticks: Some(value.into()),
            time: Some(value.into()),
        }
    }
    #[test]
    fn cadences_follow_the_binding() {
        assert_eq!(sole_interval_ms(&RecordedStart::default()), 250);
        assert_eq!(sole_interval_ms(&recorded("1")), 1_000);
        assert_eq!(identity_every_ticks(true, 1_000), 5);
        assert_eq!(identity_every_ticks(false, 1_000), 60);
        assert_eq!(
            identity_every_ticks(true, HOST_WATCH_BACKSTOP_INTERVAL_MS),
            1
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_disarmed_poll_waits_and_a_dead_supervisor_fires_on_the_first_tick() {
        let (arm, interval) = tokio::sync::watch::channel(None);
        let start = tokio::time::Instant::now();
        let watch = tokio::spawn(watch_supervisor_poll(
            u32::MAX,
            RecordedStart::default(),
            interval,
            Scripted {
                alive: false,
                start: None,
            },
        ));
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(!watch.is_finished());
        arm.send(Some(HOST_WATCH_PPID_INTERVAL_MS)).unwrap();
        let reason = watch.await.unwrap();
        assert!(reason.contains("is gone"));
        assert_eq!(
            start.elapsed(),
            Duration::from_millis(10_000 + HOST_WATCH_PPID_INTERVAL_MS)
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_changed_start_fires_only_on_the_identity_tick() {
        let (_arm, interval) = tokio::sync::watch::channel(Some(1_000));
        let start = tokio::time::Instant::now();
        let reason = watch_supervisor_poll(
            u32::MAX,
            recorded("1"),
            interval,
            Scripted {
                alive: true,
                start: Some("2".into()),
            },
        )
        .await;
        assert!(reason.contains("start time changed"));
        let every = identity_every_ticks(cfg!(target_os = "linux"), 1_000);
        assert_eq!(start.elapsed(), Duration::from_millis(1_000 * every));
    }
}
