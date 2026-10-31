//! HOW a supervisor leaves without ending work (senpi `host-lifecycle-drain.ts`): the handoff drain (SIGUSR1 to its
//! child, a soft grace that never kills busy sessions), the watch that turns a lost public entry into a drain, and the
//! win32 identity watchdog that notices a child gone without an exit event. Split out of `host_lifecycle`, which keeps
//! the supervisor's orchestration. Nothing here sends a terminating signal.
use crate::host_supervisor_log::supervisor_log;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The child's drain-request relay: the one way to reach a child on every platform (senpi `SupervisorDrain` relay).
#[derive(Debug, Clone)]
pub struct DrainRelay {
    pub file: std::path::PathBuf,
    pub sender: crate::host_stop_intent::HostStopSender,
}
/// Asks the child to drain: the relay file everywhere, plus SIGUSR1 as the immediate wake on POSIX (senpi
/// `requestChildDrain`). A child that already exited is never asked.
#[cfg(any(unix, windows))]
pub fn request_child_drain(
    child_pid: u32,
    running: &std::sync::atomic::AtomicBool,
    relay: Option<&DrainRelay>,
    windows: bool,
) {
    if !running.load(std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    if let Some(relay) = relay
        && let Err(error) = crate::host_stop_intent::write_drain_request(
            &relay.file,
            &relay.sender,
            "supervisor_drain",
        )
    {
        supervisor_log(&format!("could not relay the drain to the host: {error}"));
    }
    if windows {
        return;
    }
    #[cfg(unix)]
    if let Err(error) =
        crate::host_stop::signal_generation(child_pid, rustix::process::Signal::USR1)
    {
        supervisor_log(&format!("could not ask the host to drain: {error}"));
    }
    #[cfg(not(unix))]
    let _ = child_pid;
}
/// The signal-free drain request: the watcher is armed BEFORE the marker is advertised. A marker that cannot be
/// written only leaves this generation reachable by the POSIX signal, as before.
#[cfg(any(unix, windows))]
pub fn accept_drain_requests(
    generation: &crate::host_daemon_paths::HostGenerationPaths,
    drain_tx: tokio::sync::watch::Sender<bool>,
) -> crate::host_stop_intent::DrainRequestWatch {
    let watch = crate::host_stop_intent::watch_drain_request(
        generation.drain_request_file.clone(),
        move |request| {
            supervisor_log(&format!("drain requested ({})", request.reason));
            let _ = drain_tx.send(true);
        },
        None,
    );
    if let Err(error) =
        crate::host_stop_intent::write_drain_watch(&generation.drain_watch_file, std::process::id())
    {
        supervisor_log(&format!("cannot advertise the drain request: {error}"));
    }
    watch
}

/// senpi `SupervisorDrain`: asks the child to announce and park attached sessions as turns settle. The supervisor
/// owns the soft grace from the drain on; its expiry re-asks but NEVER kills busy sessions. Child exit ends the
/// supervisor regardless of clients that keep their connections open.
pub struct SupervisorDrain {
    draining: AtomicBool,
    child_pid: u32,
    child_running: Arc<AtomicBool>,
    shutting_down: Box<dyn Fn() -> bool + Send + Sync>,
    begin_stopping: Box<dyn Fn() + Send + Sync>,
    relay: Option<DrainRelay>,
    windows: bool,
    grace: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SupervisorDrain {
    /// `relay` is the child's drain-request file: the one way to reach a child on every platform. Without it the
    /// request is the POSIX signal alone.
    pub fn new(
        child_pid: u32,
        child_running: Arc<AtomicBool>,
        shutting_down: impl Fn() -> bool + Send + Sync + 'static,
        begin_stopping: impl Fn() + Send + Sync + 'static,
        relay: Option<DrainRelay>,
        windows: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            draining: AtomicBool::new(false),
            child_pid,
            child_running,
            shutting_down: Box::new(shutting_down),
            begin_stopping: Box::new(begin_stopping),
            relay,
            windows,
            grace: std::sync::Mutex::new(None),
        })
    }

    pub fn active(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// senpi `drain`: once, never while shutting down. The listening handle is deliberately left open: after a
    /// handoff its name is the successor's entry, and keeping it keeps the public path continuously answerable.
    pub fn drain(self: &Arc<Self>) {
        if (self.shutting_down)() || self.draining.swap(true, Ordering::SeqCst) {
            return;
        }
        (self.begin_stopping)();
        supervisor_log("draining into the next generation");
        let grace_ms = crate::host_lifecycle::resolve_handoff_grace_ms(&std::env::vars().collect());
        let this = Arc::clone(self);
        *self
            .grace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs_f64(grace_ms / 1000.)).await;
            supervisor_log(
                &serde_json::json!({"event": "handoff_grace_expired", "graceMs": grace_ms})
                    .to_string(),
            );
            this.request_child_drain();
        }));
        self.request_child_drain();
    }

    /// senpi `stop`: cancels the grace.
    pub fn stop(&self) {
        if let Some(grace) = self
            .grace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            grace.abort();
        }
    }

    /// senpi `acceptRequests`: the signal-free drain request, armed BEFORE it is advertised.
    pub fn accept_requests(
        self: &Arc<Self>,
        generation: &crate::host_daemon_paths::HostGenerationPaths,
    ) -> crate::host_stop_intent::DrainRequestWatch {
        let this = Arc::clone(self);
        let watch = crate::host_stop_intent::watch_drain_request(
            generation.drain_request_file.clone(),
            move |request| {
                supervisor_log(&format!("drain requested ({})", request.reason));
                this.drain();
            },
            None,
        );
        if let Err(error) = crate::host_stop_intent::write_drain_watch(
            &generation.drain_watch_file,
            std::process::id(),
        ) {
            supervisor_log(&format!("cannot advertise the drain request: {error}"));
        }
        watch
    }

    fn request_child_drain(&self) {
        request_child_drain(
            self.child_pid,
            &self.child_running,
            self.relay.as_ref(),
            self.windows,
        );
    }
}

/// senpi `drainOnPublicSocketLoss`: losing the public entry IS a drain request - nothing can reach this supervisor by
/// path any more, and the handoff that replaced it may never have signalled (#1893). A name that was deleted rather
/// than taken over is the same loss with nobody serving the path (#1961). A REPLACED entry names a live replacer:
/// `on_replaced` runs first, because the registration this generation would release now belongs to it (#2536).
pub fn drain_on_public_socket_loss(
    path: String,
    identity: Option<crate::socket_ownership::SocketFileIdentity>,
    settled: impl Fn() -> bool + Send + Sync + 'static,
    drain: impl FnOnce() + Send + 'static,
    on_replaced: impl FnOnce() + Send + 'static,
    pointer: Option<crate::host_supersession::SupersessionPointer>,
) -> crate::host_supersession::SupersessionWatch {
    crate::host_supersession::watch_for_supersession_with_pointer(
        path,
        identity,
        pointer,
        settled,
        move |loss| on_public_socket_loss(loss, drain, on_replaced),
    )
}

fn on_public_socket_loss(
    loss: crate::host_supersession::EndpointLoss,
    drain: impl FnOnce(),
    on_replaced: impl FnOnce(),
) {
    use crate::host_supersession::EndpointLoss;
    if loss == EndpointLoss::Replaced {
        on_replaced();
    }
    supervisor_log(match loss {
        EndpointLoss::Absent => {
            "the public socket entry is gone; nothing can reach this generation; draining"
        }
        EndpointLoss::Replaced => "another generation owns the public socket; draining this one",
    });
    drain();
}

/// The decision core of senpi `watchWin32ChildIdentity`: a child whose identity changes or disappears has exited
/// even when no exit event came. A failed baseline read is UNKNOWN, so the watch then relies on its own
/// comparisons; an absent identity is only believed once the pid probe agrees the child is gone, and only after
/// two consecutive confirmations (a loaded runner's timed-out probe means UNKNOWN, never death).
#[derive(Debug, Default)]
pub struct ChildIdentityWatch {
    baseline: Option<String>,
    missing_identity_checks: u32,
}
impl ChildIdentityWatch {
    pub fn new(baseline: Option<String>) -> Self {
        Self {
            baseline,
            missing_identity_checks: 0,
        }
    }
    /// One observation; `true` when the child is gone.
    pub fn observe(&mut self, current: Option<&str>, pid_live: bool) -> bool {
        match current {
            None if !pid_live => self.missing_identity_checks += 1,
            Some(_) => self.missing_identity_checks = 0,
            None => {}
        }
        let identity_changed = matches!((&self.baseline, current), (Some(baseline), Some(current)) if baseline != current);
        identity_changed || self.missing_identity_checks >= 2
    }
}
/// win32 only: polls the child's start identity every 500 ms until it is gone or `shutting_down` turns true, then
/// calls `on_gone` once (senpi `watchWin32ChildIdentity`). The probe is a bounded 1 s read run off the runtime.
pub fn watch_win32_child_identity(
    pid: u32,
    shutting_down: impl Fn() -> bool + Send + 'static,
    on_gone: impl FnOnce() + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let read = |pid: u32| async move {
            tokio::time::timeout(
                std::time::Duration::from_millis(1_000),
                tokio::task::spawn_blocking(move || {
                    crate::host_reservations::read_process_start_time(pid)
                }),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
        };
        let mut watch = ChildIdentityWatch::new(read(pid).await);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if shutting_down() {
                return;
            }
            let current = read(pid).await;
            let live = crate::host_reservations::process_is_live(pid);
            if watch.observe(current.as_deref(), live) {
                on_gone();
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_watch_needs_two_confirmed_absences_and_never_believes_a_live_pid() {
        let mut watch = ChildIdentityWatch::new(Some("100".into()));
        assert!(!watch.observe(Some("100"), true));
        assert!(!watch.observe(None, true));
        assert!(!watch.observe(None, false));
        // A readable identity resets the count.
        assert!(!watch.observe(Some("100"), true));
        assert!(!watch.observe(None, false));
        assert!(watch.observe(None, false));
    }
    #[test]
    fn identity_watch_reports_a_changed_identity_and_works_without_a_baseline() {
        let mut watch = ChildIdentityWatch::new(Some("100".into()));
        assert!(watch.observe(Some("200"), true));
        let mut unknown = ChildIdentityWatch::new(None);
        assert!(!unknown.observe(Some("200"), true));
        assert!(!unknown.observe(None, false));
        assert!(unknown.observe(None, false));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn drain_requests_reach_the_supervisor_without_a_signal_and_are_advertised_after_arming()
    {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let generation = crate::host_daemon_paths::generation_paths(&paths, "g");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let watch = accept_drain_requests(&generation, tx);
        assert_eq!(
            crate::host_stop_intent::read_drain_watch(&generation.drain_watch_file),
            Some(std::process::id())
        );
        let sender = crate::host_stop_intent::HostStopSender {
            pid: 1,
            kind: "stop".into(),
            generation: None,
        };
        crate::host_stop_intent::write_drain_request(
            &generation.drain_request_file,
            &sender,
            "operator_drain",
        )
        .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rx.wait_for(|drain| *drain),
        )
        .await
        .unwrap()
        .unwrap();
        watch.stop();
    }
    #[cfg(unix)]
    #[test]
    fn child_drain_is_relayed_by_file_and_skipped_once_the_child_exited() {
        let temp = tempfile::tempdir().unwrap();
        let relay = DrainRelay {
            file: temp.path().join("host-drain-request.json"),
            sender: crate::host_stop_intent::HostStopSender {
                pid: 1,
                kind: "supervisor".into(),
                generation: None,
            },
        };
        let gone = std::sync::atomic::AtomicBool::new(false);
        request_child_drain(0, &gone, Some(&relay), true);
        assert!(!relay.file.exists());
        let running = std::sync::atomic::AtomicBool::new(true);
        request_child_drain(0, &running, Some(&relay), true);
        assert_eq!(
            crate::host_stop_intent::read_drain_request(&relay.file)
                .unwrap()
                .reason,
            "supervisor_drain"
        );
    }
    #[cfg(unix)]
    fn supervisor_relay(temp: &tempfile::TempDir) -> DrainRelay {
        DrainRelay {
            file: temp.path().join("host-drain-request.json"),
            sender: crate::host_stop_intent::HostStopSender {
                pid: 1,
                kind: "supervisor".into(),
                generation: None,
            },
        }
    }
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_drain_runs_once_stops_admission_and_its_grace_only_re_asks() {
        let temp = tempfile::tempdir().unwrap();
        let relay = supervisor_relay(&temp);
        let file = relay.file.clone();
        let stopped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = stopped.clone();
        let drain = SupervisorDrain::new(
            0,
            Arc::new(AtomicBool::new(true)),
            || false,
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
            Some(relay),
            true,
        );
        assert!(!drain.active());
        drain.drain();
        drain.drain();
        assert!(drain.active());
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        let first = crate::host_stop_intent::read_drain_request(&file).unwrap();
        assert_eq!(first.reason, "supervisor_drain");
        // The grace expiry re-asks the child with a fresh request; it never kills it.
        let grace = drain.grace.lock().unwrap().take().unwrap();
        grace.await.unwrap();
        let second = crate::host_stop_intent::read_drain_request(&file).unwrap();
        assert_ne!(first.nonce, second.nonce);
    }
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_shutting_down_supervisor_never_drains_and_stop_cancels_the_grace() {
        let temp = tempfile::tempdir().unwrap();
        let shutting = SupervisorDrain::new(
            0,
            Arc::new(AtomicBool::new(true)),
            || true,
            || {},
            Some(supervisor_relay(&temp)),
            true,
        );
        shutting.drain();
        assert!(!shutting.active());
        assert!(!temp.path().join("host-drain-request.json").exists());
        let drain = SupervisorDrain::new(
            0,
            Arc::new(AtomicBool::new(true)),
            || false,
            || {},
            None,
            true,
        );
        drain.drain();
        drain.stop();
        assert!(drain.grace.lock().unwrap().is_none());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn a_drain_request_drains_the_supervisor() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let generation = crate::host_daemon_paths::generation_paths(&paths, "g");
        crate::host_daemon_paths::create_generation_directory(&generation).unwrap();
        let (began_tx, mut began) = tokio::sync::watch::channel(false);
        let drain = SupervisorDrain::new(
            0,
            Arc::new(AtomicBool::new(false)),
            || false,
            move || {
                began_tx.send_replace(true);
            },
            None,
            true,
        );
        let watch = drain.accept_requests(&generation);
        assert_eq!(
            crate::host_stop_intent::read_drain_watch(&generation.drain_watch_file),
            Some(std::process::id())
        );
        crate::host_stop_intent::write_drain_request(
            &generation.drain_request_file,
            &crate::host_stop_intent::HostStopSender {
                pid: 1,
                kind: "stop".into(),
                generation: None,
            },
            "operator_drain",
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), began.wait_for(|on| *on))
            .await
            .unwrap()
            .unwrap();
        assert!(drain.active());
        watch.stop();
    }
    #[test]
    fn a_replaced_entry_hands_the_registration_to_the_replacer_before_draining() {
        use crate::host_supersession::EndpointLoss;
        let order = std::cell::RefCell::new(Vec::new());
        on_public_socket_loss(
            EndpointLoss::Replaced,
            || order.borrow_mut().push("drain"),
            || order.borrow_mut().push("replaced"),
        );
        assert_eq!(*order.borrow(), ["replaced", "drain"]);
        order.borrow_mut().clear();
        on_public_socket_loss(
            EndpointLoss::Absent,
            || order.borrow_mut().push("drain"),
            || order.borrow_mut().push("replaced"),
        );
        assert_eq!(*order.borrow(), ["drain"]);
    }
}
