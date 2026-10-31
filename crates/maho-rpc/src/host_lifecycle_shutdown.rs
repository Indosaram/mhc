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
    /// generation's registration. `exit_code` is the code a bounded (win32) cleanup exits with.
    async fn release(&mut self, exit_code: i32) -> std::io::Result<()>;
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
        teardown.release(exit_code).await
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

/// The supervisor's signal watch: `Some(exit code)` once a shutdown signal arrived.
pub type StopRequests = tokio::sync::watch::Receiver<Option<i32>>;

/// senpi `supervisorSender`.
pub fn supervisor_sender(instance_id: &str) -> crate::host_stop_intent::HostStopSender {
    crate::host_stop_intent::HostStopSender {
        pid: std::process::id(),
        kind: "supervisor".into(),
        generation: Some(instance_id.to_owned()),
    }
}

/// senpi `childExited`: the exit is already observed.
pub fn child_exited(child: &mut tokio::process::Child) -> bool {
    !matches!(child.try_wait(), Ok(None))
}

/// senpi `waitForChildExit`: `true` once the child's exit is observed within `timeout_ms`.
pub async fn wait_for_child_exit(child: &mut tokio::process::Child, timeout_ms: u64) -> bool {
    if child_exited(child) {
        return true;
    }
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), child.wait())
        .await
        .is_ok()
}

#[async_trait::async_trait]
impl crate::host_lifecycle_stall_wait::StalledChild for tokio::process::Child {
    async fn wait_for_exit(&mut self, timeout_ms: u64) -> bool {
        wait_for_child_exit(self, timeout_ms).await
    }
}

#[cfg(unix)]
fn signal_child(pid: u32, signal: rustix::process::Signal) -> bool {
    crate::host_stop::signal_generation(pid, signal).unwrap_or(false)
}

#[cfg(unix)]
const SIGTERM: rustix::process::Signal = rustix::process::Signal::TERM;
#[cfg(unix)]
const SIGKILL: rustix::process::Signal = rustix::process::Signal::KILL;

/// senpi `stopChild`: SIGTERM, then SIGKILL after `CHILD_STOP_TIMEOUT_MS` - or, for a child that is alive but
/// stalled, after the bounded stall wait. The intent is on record BEFORE the signal, layered over any outer sender's
/// intent for this generation so the record names who started it, and names a stall-wait escalation.
#[cfg(unix)]
pub async fn stop_child(
    child: &mut tokio::process::Child,
    generation: &crate::host_daemon_paths::HostGenerationPaths,
    instance_id: &str,
    reason: &str,
) -> std::io::Result<()> {
    use crate::host_stop_intent::{
        CHILD_KILL_EXIT_TIMEOUT_MS, CHILD_STOP_TIMEOUT_MS, layered_supervisor_intent,
        read_stop_intent, write_stop_intent,
    };
    let Some(pid) = child.id() else {
        return Ok(());
    };
    if child_exited(child) {
        return Ok(());
    }
    let now = crate::host_stalled_evidence::now_ms;
    let outer = read_stop_intent(generation, now(), None);
    let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let intent = layered_supervisor_intent(
        outer.as_ref(),
        supervisor_sender(instance_id),
        pid,
        reason,
        &at,
    );
    write_stop_intent(generation, &intent, |message| supervisor_log(&message));
    let signalled_at = now();
    signal_child(pid, SIGTERM);
    if wait_for_child_exit(child, CHILD_STOP_TIMEOUT_MS).await {
        return Ok(());
    }
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let stall_wait_ms = crate::host_lifecycle_stall_wait::wait_out_stalled_child(
        &mut *child,
        generation,
        signalled_at,
        &env,
        now,
    )
    .await;
    if child_exited(child) {
        return Ok(());
    }
    if stall_wait_ms > 0 {
        let mut escalated = intent.clone();
        escalated.reason = format!(
            "{}; escalated_after_stall_wait={stall_wait_ms}",
            intent.reason
        );
        write_stop_intent(generation, &escalated, |message| supervisor_log(&message));
    }
    signal_child(pid, SIGKILL);
    // SIGKILL cannot be ignored; the bound is only a circuit breaker for an uninterruptible kernel wait.
    if !wait_for_child_exit(child, CHILD_KILL_EXIT_TIMEOUT_MS).await {
        return Err(std::io::Error::other(format!(
            "host child pid {pid} exit unobserved {CHILD_KILL_EXIT_TIMEOUT_MS}ms after SIGKILL"
        )));
    }
    Ok(())
}

/// senpi `reapChildAfterFailure`: SIGTERM (unless already sent), the stop window, then SIGKILL and its breaker.
#[cfg(unix)]
pub async fn reap_child_after_failure(
    child: &mut tokio::process::Child,
    term_attempted: bool,
) -> std::io::Result<()> {
    use crate::host_stop_intent::{CHILD_KILL_EXIT_TIMEOUT_MS, CHILD_STOP_TIMEOUT_MS};
    let Some(pid) = child.id() else {
        return Ok(());
    };
    if child_exited(child) {
        return Ok(());
    }
    if !term_attempted {
        signal_child(pid, SIGTERM);
        if wait_for_child_exit(child, CHILD_STOP_TIMEOUT_MS).await {
            return Ok(());
        }
    }
    signal_child(pid, SIGKILL);
    if !wait_for_child_exit(child, CHILD_KILL_EXIT_TIMEOUT_MS).await {
        return Err(std::io::Error::other(format!(
            "host child pid {pid} exit unobserved {CHILD_KILL_EXIT_TIMEOUT_MS}ms after recovery SIGKILL"
        )));
    }
    Ok(())
}

/// The shutdown reason a signal exit code stands for (`signal:SIGTERM` is 143, `signal:SIGHUP` 129).
pub fn signal_shutdown_reason(code: i32) -> String {
    match code {
        143 => "signal:SIGTERM".to_owned(),
        129 => "signal:SIGHUP".to_owned(),
        code => format!("signal:{code}"),
    }
}

/// Settles with the exit code of the first stop request; never settles once the signal watch is gone.
async fn stop_requested(stop: &mut StopRequests) -> i32 {
    let code = stop
        .wait_for(Option::is_some)
        .await
        .ok()
        .map(|code| (*code).unwrap_or(0));
    match code {
        Some(code) => code,
        None => std::future::pending().await,
    }
}

enum StartupInterrupted {
    Stop(i32),
    Failed(std::io::Error),
}

async fn shutdown_startup(
    teardown: &mut dyn SupervisorTeardown,
    state: &SupervisorState,
    interrupted: StartupInterrupted,
) -> i32 {
    match interrupted {
        StartupInterrupted::Stop(code) => {
            perform_shutdown(teardown, state, &signal_shutdown_reason(code), code).await
        }
        StartupInterrupted::Failed(cause) => {
            perform_shutdown(teardown, state, &format!("startup failed: {cause}"), 1).await
        }
    }
}

/// senpi `runHostSupervisor` startup: the signals are already registered, so a stop arriving during readiness or
/// startup publication runs the same shutdown instead of the default kill, and the shutdown joins publication
/// before it removes what publication writes into. Every startup failure is routed through [`perform_shutdown`]
/// (`startup failed: ...`, exit 1), so the child is stopped and the endpoint key disposed. Answers what
/// publication produced, or the exit code of the shutdown that ended startup.
pub async fn drive_startup<T, P>(
    state: &std::sync::Arc<SupervisorState>,
    teardown: &mut T,
    stop: &mut StopRequests,
    readiness: impl AsyncFnOnce(&mut T) -> std::io::Result<()>,
    publication: impl std::future::Future<Output = std::io::Result<P>>,
) -> Result<P, i32>
where
    T: SupervisorTeardown,
{
    let ready = {
        let readiness = readiness(&mut *teardown);
        tokio::select! {
            biased;
            code = stop_requested(stop) => Err(StartupInterrupted::Stop(code)),
            result = readiness => result.map_err(StartupInterrupted::Failed),
        }
    };
    if let Err(interrupted) = ready {
        return Err(shutdown_startup(&mut *teardown, state, interrupted).await);
    }
    // senpi `if (state.shuttingDown) return`: a stop that is already pending publishes nothing.
    let pending = *stop.borrow();
    if let Some(code) = pending {
        return Err(shutdown_startup(&mut *teardown, state, StartupInterrupted::Stop(code)).await);
    }
    // Only this startup begins shutdown before the supervisor loop, so publication can always begin here.
    let Some(publishing) = state.begin_startup() else {
        return Err(0);
    };
    let publication = async move {
        let _publishing = publishing;
        publication.await
    };
    tokio::pin!(publication);
    let outcome = tokio::select! {
        biased;
        code = stop_requested(stop) => Err(code),
        result = &mut publication => Ok(result),
    };
    let published = match outcome {
        Err(code) => {
            // Stop the child at once; the shutdown joins the publication writes still in flight.
            let reason = signal_shutdown_reason(code);
            let (_, exit_code) = tokio::join!(
                &mut publication,
                perform_shutdown(&mut *teardown, state, &reason, code)
            );
            return Err(exit_code);
        }
        Ok(Err(cause)) => {
            return Err(
                shutdown_startup(&mut *teardown, state, StartupInterrupted::Failed(cause)).await,
            );
        }
        Ok(Ok(published)) => published,
    };
    // A stop that arrived while publication ran without yielding is honoured before the host is announced.
    let stopped = *stop.borrow();
    if let Some(code) = stopped {
        drop(published);
        return Err(shutdown_startup(&mut *teardown, state, StartupInterrupted::Stop(code)).await);
    }
    Ok(published)
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
        async fn release(&mut self, _exit_code: i32) -> std::io::Result<()> {
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

    fn fake(recorded: &std::sync::Arc<Recorded>) -> FakeTeardown {
        FakeTeardown {
            recorded: recorded.clone(),
            release_fails: false,
        }
    }

    const FULL_SHUTDOWN: [&str; 6] = [
        "stop_admission",
        "stop_child",
        "child_exit_recorded",
        "stop_watchers",
        "release",
        "dispose_public_endpoint",
    ];

    /// senpi#4216: the signals are registered before startup, so a SIGTERM arriving while startup publication writes
    /// runs the shutdown; it stops the child at once and releases the generation only after publication stopped.
    #[tokio::test]
    async fn a_stop_during_startup_publication_runs_the_shutdown_and_joins_publication() {
        let state = std::sync::Arc::new(SupervisorState::default());
        let recorded = std::sync::Arc::new(Recorded::default());
        let mut teardown = fake(&recorded);
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(None);
        let (began_tx, began) = tokio::sync::oneshot::channel::<()>();
        let (finish_tx, finish) = tokio::sync::oneshot::channel::<()>();
        let child_stopped = recorded.child_stopped.notified();
        let startup = drive_startup(
            &state,
            &mut teardown,
            &mut stop_rx,
            async |_: &mut FakeTeardown| -> std::io::Result<()> { Ok(()) },
            async move {
                let _ = began_tx.send(());
                let _ = finish.await;
                std::io::Result::Ok("published")
            },
        );
        let driver = async {
            began.await.unwrap();
            stop_tx.send_replace(Some(143));
            child_stopped.await;
            assert!(state.shutting_down());
            assert!(!recorded.steps.lock().unwrap().contains(&"release"));
            finish_tx.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(startup, driver);
        assert_eq!(result, Err(143));
        assert_eq!(*recorded.steps.lock().unwrap(), FULL_SHUTDOWN);
        assert_eq!(state.shutdown_reason().as_deref(), Some("signal:SIGTERM"));
    }

    /// A stop while the child is still becoming ready shuts down and never begins publication.
    #[tokio::test]
    async fn a_stop_before_readiness_shuts_down_without_publishing() {
        let state = std::sync::Arc::new(SupervisorState::default());
        let recorded = std::sync::Arc::new(Recorded::default());
        let mut teardown = fake(&recorded);
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(None);
        let (probing_tx, probing) = tokio::sync::oneshot::channel::<()>();
        let published = std::sync::atomic::AtomicBool::new(false);
        let startup = drive_startup(
            &state,
            &mut teardown,
            &mut stop_rx,
            async move |_: &mut FakeTeardown| -> std::io::Result<()> {
                let _ = probing_tx.send(());
                std::future::pending().await
            },
            async {
                published.store(true, std::sync::atomic::Ordering::SeqCst);
                std::io::Result::Ok(())
            },
        );
        let driver = async {
            probing.await.unwrap();
            stop_tx.send_replace(Some(129));
        };
        let (result, ()) = tokio::join!(startup, driver);
        assert_eq!(result, Err(129));
        assert!(!published.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(*recorded.steps.lock().unwrap(), FULL_SHUTDOWN);
        assert_eq!(state.shutdown_reason().as_deref(), Some("signal:SIGHUP"));
    }

    /// A failed bind/chmod/publish/adopt is `startup failed`: the child is stopped and the key record disposed.
    #[tokio::test]
    async fn a_startup_publication_failure_stops_the_child_and_disposes_the_key() {
        let state = std::sync::Arc::new(SupervisorState::default());
        let recorded = std::sync::Arc::new(Recorded::default());
        let (_stop_tx, mut stop_rx) = tokio::sync::watch::channel(None);
        let result: Result<(), i32> = drive_startup(
            &state,
            &mut fake(&recorded),
            &mut stop_rx,
            async |_: &mut FakeTeardown| -> std::io::Result<()> { Ok(()) },
            async { Err(std::io::Error::other("bind refused")) },
        )
        .await;
        assert_eq!(result, Err(1));
        assert_eq!(*recorded.steps.lock().unwrap(), FULL_SHUTDOWN);
        assert_eq!(
            state.shutdown_reason().as_deref(),
            Some("startup failed: bind refused")
        );
    }

    /// A readiness failure (child exited, timed out, registration unwritable) is routed the same way.
    #[tokio::test]
    async fn a_readiness_failure_stops_the_child_through_the_shutdown() {
        let published = std::sync::atomic::AtomicBool::new(false);
        let state = std::sync::Arc::new(SupervisorState::default());
        let recorded = std::sync::Arc::new(Recorded::default());
        let (_stop_tx, mut stop_rx) = tokio::sync::watch::channel(None);
        let result: Result<(), i32> = drive_startup(
            &state,
            &mut fake(&recorded),
            &mut stop_rx,
            async |_: &mut FakeTeardown| -> std::io::Result<()> {
                Err(std::io::Error::other(
                    "supervised host did not become ready",
                ))
            },
            async {
                published.store(true, std::sync::atomic::Ordering::SeqCst);
                std::io::Result::Ok(())
            },
        )
        .await;
        assert_eq!(result, Err(1));
        assert!(!published.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(*recorded.steps.lock().unwrap(), FULL_SHUTDOWN);
    }

    /// Without a stop, startup answers what publication produced and nothing is torn down.
    #[tokio::test]
    async fn an_undisturbed_startup_answers_the_publication() {
        let state = std::sync::Arc::new(SupervisorState::default());
        let recorded = std::sync::Arc::new(Recorded::default());
        let (_stop_tx, mut stop_rx) = tokio::sync::watch::channel(None);
        let result = drive_startup(
            &state,
            &mut fake(&recorded),
            &mut stop_rx,
            async |_: &mut FakeTeardown| -> std::io::Result<()> { Ok(()) },
            async { std::io::Result::Ok(7) },
        )
        .await;
        assert_eq!(result, Ok(7));
        assert!(recorded.steps.lock().unwrap().is_empty());
        assert!(!state.shutting_down());
    }

    #[test]
    fn the_supervisor_sender_names_this_process_and_its_generation() {
        let sender = supervisor_sender("gen-1");
        assert_eq!(sender.pid, std::process::id());
        assert_eq!(sender.kind, "supervisor");
        assert_eq!(sender.generation.as_deref(), Some("gen-1"));
    }

    /// senpi `stopChild`: the supervisor's intent is on record before the SIGTERM that ends the child.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_child_records_the_intent_then_terminates_the_child() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let generation = crate::host_daemon_paths::generation_paths(&paths, "gen-1");
        std::fs::create_dir_all(&generation.dir).unwrap();
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        assert!(!child_exited(&mut child));
        stop_child(&mut child, &generation, "gen-1", "signal:SIGTERM")
            .await
            .unwrap();
        assert!(child_exited(&mut child));
        assert!(wait_for_child_exit(&mut child, 0).await);
        let intent = crate::host_stop_intent::read_stop_intent(
            &generation,
            crate::host_stalled_evidence::now_ms(),
            None,
        )
        .unwrap();
        assert_eq!(intent.target_pid, pid);
        assert_eq!(intent.reason, "signal:SIGTERM");
        assert_eq!(intent.sender, supervisor_sender("gen-1"));
        // An exited child is never signalled again.
        stop_child(&mut child, &generation, "gen-1", "again")
            .await
            .unwrap();
        reap_child_after_failure(&mut child, false).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_shutdown_reaps_the_child() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        reap_child_after_failure(&mut child, false).await.unwrap();
        assert!(child_exited(&mut child));
    }
}
