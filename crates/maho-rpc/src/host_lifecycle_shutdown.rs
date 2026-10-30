//! HOW the supervisor ends its generation (senpi `host-lifecycle-shutdown.ts`): the single teardown, the child
//! stop, and the order that keeps the record of the child's end ahead of the release of the directory it cites.
//!
//! Startup publication (bind, key record, ownership token, drain watch) runs after the public listener is already
//! reachable, so a stop can arrive while it still writes. Shutdown stops the child at once, but joins that
//! publication before it stops the watchers startup installed and removes the directories it writes into
//! (senpi#4216, #4242). The endpoint key record is disposed last, whatever failed before it.
use crate::host_supervisor_log::{supervisor_log, write_stderr_line};

/// The supervisor's shutdown-relevant state (senpi `SupervisorState`).
pub struct SupervisorState {
    shutting_down: std::sync::atomic::AtomicBool,
    reason: std::sync::Mutex<Option<String>>,
    startup_settled: tokio::sync::watch::Sender<bool>,
}

impl Default for SupervisorState {
    fn default() -> Self {
        // Nothing to join until startup publication begins (senpi `startupComplete: Promise.resolve()`).
        let (startup_settled, _) = tokio::sync::watch::channel(true);
        Self {
            shutting_down: Default::default(),
            reason: Default::default(),
            startup_settled,
        }
    }
}

/// Held by startup publication while it writes; dropping it (success, failure or cancellation) settles the join.
pub struct StartupPublication(std::sync::Arc<SupervisorState>);

impl Drop for StartupPublication {
    fn drop(&mut self) {
        self.0.startup_settled.send_replace(true);
    }
}

impl SupervisorState {
    pub fn shutting_down(&self) -> bool {
        self.shutting_down.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn shutdown_reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Marks the start of startup publication; `None` once shutdown began (senpi `if (state.shuttingDown) return`).
    pub fn begin_startup(self: &std::sync::Arc<Self>) -> Option<StartupPublication> {
        if self.shutting_down() {
            return None;
        }
        self.startup_settled.send_replace(false);
        Some(StartupPublication(std::sync::Arc::clone(self)))
    }

    /// Waits until startup publication stopped writing; its own failure is reported by the startup caller.
    pub async fn join_startup(&self) {
        let mut settled = self.startup_settled.subscribe();
        let _ = settled.wait_for(|settled| *settled).await;
    }

    fn begin_shutdown(&self, reason: &str) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *self
            .reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.to_owned());
    }
}

/// The resources one generation tears down, in the order [`perform_shutdown`] calls them.
#[async_trait::async_trait]
pub trait SupervisorTeardown: Send {
    /// Stops admitting new public clients and the drain relay.
    fn stop_admission(&mut self);
    /// SIGTERM, then SIGKILL past the bound; fails when the child's exit could not be observed.
    async fn stop_child(&mut self, reason: &str) -> std::io::Result<()>;
    /// Settles once the child's end is on record.
    async fn child_exit_recorded(&mut self);
    /// Stops the idle ticker, the supersession watch, the drain-request watch and the win32 identity watchdog.
    fn stop_watchers(&mut self);
    /// Closes the listener, removes the internal hop directory, unlinks the owned public entry and releases the
    /// generation's registration.
    async fn release(&mut self) -> std::io::Result<()>;
    /// Kills a child the failed shutdown left behind.
    async fn reap_child_after_failure(&mut self) -> std::io::Result<()>;
    /// Removes this launch's endpoint key record.
    fn dispose_public_endpoint(&mut self) -> std::io::Result<()>;
}

/// senpi `performShutdown`, minus the final `process.exit`: answers the exit code the caller exits with.
pub async fn perform_shutdown(
    teardown: &mut dyn SupervisorTeardown,
    state: &SupervisorState,
    reason: &str,
    exit_code: i32,
) -> i32 {
    state.begin_shutdown(reason);
    teardown.stop_admission();
    write_stderr_line(&format!("senpi rpc host supervisor: {reason} shutdown"));
    let mut exit_code = exit_code;
    let mut child_stop_started = false;
    let result: std::io::Result<()> = async {
        // No fallible socket/metadata cleanup may bypass the child's observed exit.
        child_stop_started = true;
        teardown.stop_child(reason).await?;
        teardown.child_exit_recorded().await;
        // The startup caller reports its failure; this join only waits until its writes have stopped.
        state.join_startup().await;
        teardown.stop_watchers();
        teardown.release().await
    }
    .await;
    if let Err(cause) = result {
        // An unobserved child exit leaves its generation, pointer and settings intact.
        supervisor_log(&format!("shutdown failed for host child: {cause}"));
        exit_code = 1;
        let recovery = async {
            if !child_stop_started {
                teardown.reap_child_after_failure().await?;
            }
            teardown.child_exit_recorded().await;
            std::io::Result::Ok(())
        }
        .await;
        if let Err(cause) = recovery {
            supervisor_log(&format!("shutdown recovery failed for host child: {cause}"));
        }
    }
    if let Err(cause) = teardown.dispose_public_endpoint() {
        supervisor_log(&format!("endpoint key cleanup failed: {cause}"));
    }
    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorded {
        steps: std::sync::Mutex<Vec<&'static str>>,
        child_stopped: tokio::sync::Notify,
    }

    struct FakeTeardown {
        recorded: std::sync::Arc<Recorded>,
        release_fails: bool,
    }

    impl FakeTeardown {
        fn note(&self, step: &'static str) {
            self.recorded.steps.lock().unwrap().push(step);
        }
    }

    #[async_trait::async_trait]
    impl SupervisorTeardown for FakeTeardown {
        fn stop_admission(&mut self) {
            self.note("stop_admission");
        }
        async fn stop_child(&mut self, _reason: &str) -> std::io::Result<()> {
            self.note("stop_child");
            self.recorded.child_stopped.notify_one();
            Ok(())
        }
        async fn child_exit_recorded(&mut self) {
            self.note("child_exit_recorded");
        }
        fn stop_watchers(&mut self) {
            self.note("stop_watchers");
        }
        async fn release(&mut self) -> std::io::Result<()> {
            self.note("release");
            if self.release_fails {
                return Err(std::io::Error::other("release refused"));
            }
            Ok(())
        }
        async fn reap_child_after_failure(&mut self) -> std::io::Result<()> {
            self.note("reap");
            Ok(())
        }
        fn dispose_public_endpoint(&mut self) -> std::io::Result<()> {
            self.note("dispose_public_endpoint");
            Ok(())
        }
    }

    /// senpi#4216/#4242: the generation is released only after startup publication stopped writing, and the child is
    /// stopped without waiting for it.
    #[tokio::test]
    async fn shutdown_stops_the_child_at_once_and_joins_startup_before_releasing() {
        let state = std::sync::Arc::new(SupervisorState::default());
        let recorded = std::sync::Arc::new(Recorded::default());
        let (release_startup, startup_released) = tokio::sync::oneshot::channel::<()>();
        let publication_state = state.clone();
        let (startup_began, began) = tokio::sync::oneshot::channel::<()>();
        let publication = tokio::spawn(async move {
            let _publication = publication_state
                .begin_startup()
                .expect("not shutting down yet");
            let _ = startup_began.send(());
            let _ = startup_released.await;
        });
        began.await.unwrap();
        let child_stopped = recorded.child_stopped.notified();
        let shutdown_state = state.clone();
        let mut teardown = FakeTeardown {
            recorded: recorded.clone(),
            release_fails: false,
        };
        let shutdown = tokio::spawn(async move {
            perform_shutdown(&mut teardown, &shutdown_state, "signal:SIGTERM", 143).await
        });
        child_stopped.await;
        assert!(state.shutting_down());
        assert!(state.begin_startup().is_none());
        assert!(!recorded.steps.lock().unwrap().contains(&"release"));
        release_startup.send(()).unwrap();
        publication.await.unwrap();
        assert_eq!(shutdown.await.unwrap(), 143);
        assert_eq!(
            *recorded.steps.lock().unwrap(),
            [
                "stop_admission",
                "stop_child",
                "child_exit_recorded",
                "stop_watchers",
                "release",
                "dispose_public_endpoint"
            ]
        );
        assert_eq!(state.shutdown_reason().as_deref(), Some("signal:SIGTERM"));
    }

    #[tokio::test]
    async fn a_failed_release_exits_1_and_still_disposes_the_endpoint_key() {
        let state = std::sync::Arc::new(SupervisorState::default());
        let recorded = std::sync::Arc::new(Recorded::default());
        let mut teardown = FakeTeardown {
            recorded: recorded.clone(),
            release_fails: true,
        };
        assert_eq!(
            perform_shutdown(&mut teardown, &state, "startup failed: x", 1).await,
            1
        );
        let steps = recorded.steps.lock().unwrap();
        assert_eq!(steps.last(), Some(&"dispose_public_endpoint"));
        // The child stop had started, so recovery only re-awaits its exit record.
        assert!(!steps.contains(&"reap"));
    }
}
