//! IS-6 (senpi `host-lifecycle-stall-wait.ts`, senpi#2566): a graceful stop of a host child that is alive but
//! measurably stalled WAITS for it instead of SIGKILLing it after five seconds - a stalled host is still serving, and
//! the SIGTERM it was sent is handled the moment its loop runs again. The wait is bounded
//! (`SENPI_RPC_CHILD_STALLED_STOP_MAX_MS`), re-reads the evidence every slice, and is reported in `stop-progress.json`
//! so a caller holding a deadline on this supervisor extends it instead of killing the supervisor mid-wait.
use crate::host_daemon_paths::HostGenerationPaths;
use crate::host_stalled_evidence::{
    CHILD_STALLED_STOP_MAX_MS_ENV, LoopStallWindow, STOP_STALL_EVIDENCE_MAX_AGE_MS, StopProgress,
    child_stalled_stop_max_ms, clear_stop_progress, host_loop_stalled, write_stop_progress,
};
use crate::host_supervisor_log::supervisor_log;
use std::collections::HashMap;

const STALL_WAIT_SLICE_MS: i64 = 5_000;

/// senpi `loopLagErrorMs`: the drift past which a tick is a stall, as the host itself judges it.
pub fn loop_lag_error_ms(env: &HashMap<String, String>) -> i64 {
    crate::host_lifecycle::parse_idle_exit_ms(
        env.get(crate::loop_lag_watchdog::LOOP_LAG_ERROR_MS_ENV)
            .map(String::as_str),
    )
    .unwrap_or(crate::loop_lag_watchdog::DEFAULT_LOOP_LAG_ERROR_MS) as i64
}

fn iso(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The child the wait is for (senpi `waitForExit(child, timeoutMs)`): whether it exited within `timeout_ms`.
#[async_trait::async_trait]
pub trait StalledChild: Send {
    async fn wait_for_exit(&mut self, timeout_ms: u64) -> bool;
}

/// senpi `waitOutStalledChild`: waits past the ordinary stop window while the child stays stalled, in slices, up to
/// the bound measured from `signalled_at`. Returns how long it waited beyond the ordinary window (0 when the child was
/// not stalled at all, so the ordinary escalation is unchanged). `now` is the epoch-ms clock.
pub async fn wait_out_stalled_child(
    child: &mut dyn StalledChild,
    generation: &HostGenerationPaths,
    signalled_at: i64,
    env: &HashMap<String, String>,
    now: impl Fn() -> i64,
) -> i64 {
    let max_ms = child_stalled_stop_max_ms(env) as i64;
    let until_at = signalled_at + max_ms;
    let error_ms = loop_lag_error_ms(env);
    let stalled = || {
        host_loop_stalled(
            generation,
            LoopStallWindow {
                now: now(),
                error_ms,
                window_ms: STOP_STALL_EVIDENCE_MAX_AGE_MS as i64,
            },
        )
    };
    let mut waited = 0;
    while now() < until_at && stalled() {
        if waited == 0 {
            supervisor_log(&format!(
                "host child is stalled; waiting up to {max_ms}ms ({CHILD_STALLED_STOP_MAX_MS_ENV})"
            ));
        }
        let _ = write_stop_progress(
            generation,
            &StopProgress {
                at: iso(now()),
                until_at: iso(until_at),
            },
        );
        let slice = STALL_WAIT_SLICE_MS.min(until_at - now());
        let exited = child.wait_for_exit(slice.max(0) as u64).await;
        waited += slice;
        if exited {
            break;
        }
    }
    clear_stop_progress(generation);
    waited
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    const NOW: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z

    fn generation(temp: &tempfile::TempDir) -> HostGenerationPaths {
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let generation = crate::host_daemon_paths::generation_paths(&paths, "g");
        std::fs::create_dir_all(&generation.dir).unwrap();
        generation
    }

    fn stall(generation: &HostGenerationPaths, at: i64) {
        std::fs::write(
            &generation.stalled_file,
            serde_json::json!({"at": iso(at), "driftMs": 6000, "processCpuMs": 5900, "heapDeltaMb": 0})
                .to_string(),
        )
        .unwrap();
    }

    /// A child on a virtual clock: each slice advances the clock by its length; it exits on slice `exit_on`.
    struct FakeChild {
        clock: Arc<AtomicI64>,
        generation: HostGenerationPaths,
        slices: Vec<u64>,
        exit_on: Option<usize>,
        /// The host keeps writing fresh stall evidence while it is stalled.
        restall: bool,
    }

    #[async_trait::async_trait]
    impl StalledChild for FakeChild {
        async fn wait_for_exit(&mut self, timeout_ms: u64) -> bool {
            let progress: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&self.generation.stop_progress_file).unwrap(),
            )
            .unwrap();
            assert_eq!(progress["phase"], "waiting_stalled_child");
            self.slices.push(timeout_ms);
            if self.restall {
                stall(&self.generation, self.clock.load(Ordering::SeqCst));
            }
            self.clock.fetch_add(timeout_ms as i64, Ordering::SeqCst);
            self.exit_on == Some(self.slices.len())
        }
    }

    fn fake(generation: &HostGenerationPaths, clock: &Arc<AtomicI64>) -> FakeChild {
        FakeChild {
            clock: clock.clone(),
            generation: generation.clone(),
            slices: Vec::new(),
            exit_on: None,
            restall: false,
        }
    }

    /// A child that is not stalled leaves the ordinary escalation unchanged: no wait, no progress report.
    #[tokio::test]
    async fn a_child_that_is_not_stalled_is_not_waited_for() {
        let temp = tempfile::tempdir().unwrap();
        let generation = generation(&temp);
        let clock = Arc::new(AtomicI64::new(NOW));
        let mut child = fake(&generation, &clock);
        let waited = wait_out_stalled_child(&mut child, &generation, NOW, &HashMap::new(), || {
            clock.load(Ordering::SeqCst)
        })
        .await;
        assert_eq!(waited, 0);
        assert!(child.slices.is_empty());
        assert!(!generation.stop_progress_file.exists());
    }

    /// senpi rpc-host-stalled-stop "waits out the stall and lets the host exit on the SIGTERM it was sent": slices
    /// are waited while the stall persists, the progress report is visible during the wait and removed after it.
    #[tokio::test]
    async fn a_stalled_child_is_waited_for_in_slices_until_it_exits() {
        let temp = tempfile::tempdir().unwrap();
        let generation = generation(&temp);
        stall(&generation, NOW);
        let clock = Arc::new(AtomicI64::new(NOW));
        let mut child = fake(&generation, &clock);
        child.exit_on = Some(3);
        let waited = wait_out_stalled_child(&mut child, &generation, NOW, &HashMap::new(), || {
            clock.load(Ordering::SeqCst)
        })
        .await;
        assert_eq!(waited, 15_000);
        assert_eq!(child.slices, [5_000, 5_000, 5_000]);
        assert!(!generation.stop_progress_file.exists());
    }

    /// senpi rpc-host-stalled-stop "escalates once the stall outlasts the bounded wait": the wait ends at the bound
    /// measured from the signal, the last slice clipped to it.
    #[tokio::test]
    async fn the_wait_is_bounded_from_the_signal() {
        let temp = tempfile::tempdir().unwrap();
        let generation = generation(&temp);
        let env = HashMap::from([(CHILD_STALLED_STOP_MAX_MS_ENV.to_owned(), "12000".to_owned())]);
        let clock = Arc::new(AtomicI64::new(NOW + 5_000));
        stall(&generation, NOW + 5_000);
        let mut child = fake(&generation, &clock);
        child.restall = true;
        let waited = wait_out_stalled_child(&mut child, &generation, NOW, &env, || {
            clock.load(Ordering::SeqCst)
        })
        .await;
        assert_eq!(waited, 7_000);
        assert_eq!(child.slices, [5_000, 2_000]);
        assert_eq!(clock.load(Ordering::SeqCst), NOW + 12_000);
        assert!(!generation.stop_progress_file.exists());
    }
}
