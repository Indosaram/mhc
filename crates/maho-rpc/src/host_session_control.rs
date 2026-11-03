//! `pi.session.registerControlEndpoint` for a session a multi-session host serves, and the host's `wake` command for it
//! (senpi `host-session-control.ts`).
//!
//! The host's public socket already is the session's endpoint - a caller reaches it with `wake { sessionId }` - so
//! registering binds nothing: it installs the registrant's drain and wakes it on the edges a terminal endpoint uses:
//! `agent_idle`, an admitted delivery reaching the transcript, an inbox entry, and the `wake` command. One pass runs
//! at a time, so the answer to `wake` is the outcome of the pass that covered it.
//!
//! A host with no public socket (stdio) has nothing another process can reach, so a registration there answers
//! `unsupported_mode`. `wake` always answers: with no drain registered it still emits `session_control_wake` into the
//! session's extensions and reports `admitted: []`.
//!
//! Every wake runs a pass, so a drain failure that repeats is reported once (`control drain failed: <reason>`) until a
//! pass succeeds again (senpi#1df92e349e).
use crate::session_control_wake::{
    DrainResult, InboxWatch, WakeScheduler, empty_result, watch_inbox,
};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A listener registration the session hands back; dropping it unsubscribes.
pub type EdgeSubscription = Box<dyn Send>;

/// What `HostSessionControl` needs from the hosted session (senpi `AgentSession` surface it reads).
pub trait ControlSession: Send + Sync + 'static {
    /// `session.extensionRunner.emit(event)` for a `session_control_wake` event.
    fn emit_wake(&self, event: Value) -> BoxFuture<'static, ()>;
    /// `session.subscribe` filtered to `agent_idle`.
    fn on_idle(&self, listener: Arc<dyn Fn() + Send + Sync>) -> EdgeSubscription;
    /// `session.externalAdmission.onEmitted`.
    fn on_emitted(&self, listener: Arc<dyn Fn() + Send + Sync>) -> EdgeSubscription;
}

/// The registrant's drain (senpi `RegisterControlEndpointOptions.drain`); `Ok(None)` is `admitted: []`.
pub type ControlDrain =
    Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Option<DrainResult>, String>> + Send + Sync>;

/// `SessionControlRegistration`.
pub enum ControlRegistration {
    Registered { socket: String, registration: u64 },
    Unsupported { reason: &'static str },
    Failed { reason: String },
}

struct Active {
    id: u64,
    scheduler: WakeScheduler,
    _idle: EdgeSubscription,
    _emitted: EdgeSubscription,
    inbox: InboxWatch,
}

impl Active {
    fn release(self) {
        self.inbox.stop();
        self.scheduler.dispose();
    }
}

#[derive(Default)]
struct State {
    active: Option<Active>,
    disposed: bool,
    next_id: u64,
}

pub struct HostSessionControl {
    session: Arc<dyn ControlSession>,
    socket: Option<String>,
    report: Arc<dyn Fn(String) + Send + Sync>,
    state: Arc<Mutex<State>>,
}

/// senpi `INBOX_ENGINE_ENTRY_PREFIX`: the engine's own inbox entries never count as deliveries.
pub use crate::session_control_wake::INBOX_ENGINE_ENTRY_PREFIX;

impl HostSessionControl {
    pub fn new(
        session: Arc<dyn ControlSession>,
        socket: Option<String>,
        report: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Self {
        Self {
            session,
            socket,
            report,
            state: Arc::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The scheduler whose failures are reported once per distinct reason, until a pass succeeds again.
    fn scheduler(&self, drain: ControlDrain) -> WakeScheduler {
        let failing: Arc<Mutex<Option<String>>> = Arc::default();
        let cleared = failing.clone();
        let session = self.session.clone();
        let report = self.report.clone();
        WakeScheduler::new(
            Arc::new(move |event: Value| {
                let session = session.clone();
                let drain = drain.clone();
                let cleared = cleared.clone();
                Box::pin(async move {
                    session.emit_wake(event.clone()).await;
                    let result = drain(event).await?.unwrap_or_else(empty_result);
                    *cleared
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    Ok(result)
                })
            }),
            Arc::new(move |reason: String| {
                let mut failing = failing
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if failing.as_deref() == Some(reason.as_str()) {
                    return;
                }
                *failing = Some(reason.clone());
                report(format!("control drain failed: {reason}"));
            }),
        )
    }

    /// senpi `register`: replaces any earlier registration; `inbox_dir` is watched for new entries.
    ///
    /// Passes run on the tokio runtime current at the call. Called outside one, it answers
    /// `Failed { reason: "no tokio runtime" }` and leaves any earlier registration in place.
    pub async fn register(
        &self,
        inbox_dir: &std::path::Path,
        drain: ControlDrain,
    ) -> ControlRegistration {
        if tokio::runtime::Handle::try_current().is_err() {
            return ControlRegistration::Failed {
                reason: "no tokio runtime".to_owned(),
            };
        }
        self.release_active();
        let Some(socket) = self.socket.clone().filter(|_| !self.state().disposed) else {
            return ControlRegistration::Unsupported {
                reason: "unsupported_mode",
            };
        };
        let scheduler = self.scheduler(drain);
        let wake = |reason: &'static str| {
            let scheduler = scheduler.clone();
            Arc::new(move || drop(scheduler.request(reason, &[]))) as Arc<dyn Fn() + Send + Sync>
        };
        let idle = self.session.on_idle(wake("idle"));
        let emitted = self.session.on_emitted(wake("emitted"));
        let inbox_wake = wake("inbox");
        let mut inbox = match watch_inbox(inbox_dir, inbox_wake, {
            let report = self.report.clone();
            Arc::new(move |error: String| report(format!("control inbox watch failed: {error}")))
        })
        .await
        {
            Ok(inbox) => inbox,
            Err(reason) => {
                drop((idle, emitted));
                scheduler.dispose();
                return ControlRegistration::Failed { reason };
            }
        };
        let mut state = self.state();
        // A rebind may dispose this host while the watch arms; a registration it outlived stays inert.
        if state.disposed {
            drop(state);
            inbox.stop();
            scheduler.dispose();
            return ControlRegistration::Unsupported {
                reason: "unsupported_mode",
            };
        }
        state.next_id += 1;
        let id = state.next_id;
        let armed = std::mem::replace(&mut inbox.armed, Box::pin(async {}));
        if let Some(previous) = state.active.replace(Active {
            id,
            scheduler: scheduler.clone(),
            _idle: idle,
            _emitted: emitted,
            inbox,
        }) {
            previous.release();
        }
        drop(state);
        // Anything that reached the inbox before this point is picked up by this first pass, and anything written
        // after it but before the watch was armed (no event for it) by the pass once arming settled. A disposed
        // scheduler answers that second request at once.
        drop(scheduler.request("inbox", &[]));
        tokio::spawn(async move {
            armed.await;
            drop(scheduler.request("inbox", &[]));
        });
        ControlRegistration::Registered {
            socket,
            registration: id,
        }
    }

    fn dispose_state(state: &Mutex<State>, registration: u64) {
        let released = {
            let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.id == registration)
            {
                state.active.take()
            } else {
                None
            }
        };
        if let Some(active) = released {
            active.release();
        }
    }

    /// The registration's `dispose`: releases it only while it is still the active one.
    pub fn dispose_registration(&self, registration: u64) {
        Self::dispose_state(&self.state, registration);
    }

    /// The `wake` command: one drain pass covering `delivery_ids`, answered with what it admitted.
    pub async fn wake(&self, delivery_ids: &[String]) -> DrainResult {
        let scheduler = self
            .state()
            .active
            .as_ref()
            .map(|active| active.scheduler.clone());
        if let Some(scheduler) = scheduler {
            return scheduler.request("command", delivery_ids).await;
        }
        let mut event =
            json!({"type":"session_control_wake","reason":"command","reasons":["command"]});
        if !delivery_ids.is_empty() {
            event["delivery_ids"] = json!(delivery_ids);
        }
        self.session.emit_wake(event).await;
        empty_result()
    }

    /// Synchronous so a rebind never gains an await: every edge source stops here.
    pub fn dispose(&self) {
        self.state().disposed = true;
        self.release_active();
    }

    fn release_active(&self) {
        let active = self.state().active.take();
        if let Some(active) = active {
            active.release();
        }
    }
}

/// senpi `runtimeHost.launchProfile?.sessionContext?.host_socket`: the public socket the session was opened on.
pub fn profile_host_socket(
    runtime: &maho_core::agent_session_runtime::AgentSessionRuntime,
) -> Option<String> {
    runtime.holder_endpoint()?.host_socket.clone()
}

/// The live `AgentSession` a host serves, as the control edges read it.
pub struct AgentControlSession(pub maho_core::agent_session::AgentSession);

impl ControlSession for AgentControlSession {
    fn emit_wake(&self, event: Value) -> BoxFuture<'static, ()> {
        let session = self.0.clone();
        Box::pin(async move {
            // senpi `extensionRunner.emit(event)`: an extension handler failure is the runner's own report and
            // never fails the pass.
            if let Err(error) = session.emit_session_control_wake(&event).await {
                eprintln!("senpi rpc session control wake: {error}");
            }
        })
    }
    fn on_idle(&self, listener: Arc<dyn Fn() + Send + Sync>) -> EdgeSubscription {
        Box::new(self.0.subscribe(Arc::new(move |event| {
            if matches!(event, maho_ext_api::AgentSessionEvent::AgentIdle) {
                listener();
            }
        })))
    }
    fn on_emitted(&self, listener: Arc<dyn Fn() + Send + Sync>) -> EdgeSubscription {
        Box::new(
            self.0
                .external_admission()
                .on_emitted(Arc::new(move |_delivery| listener())),
        )
    }
}

/// senpi `HostSessionControl implements ControlEndpointHost`: what `pi.session.registerControlEndpoint` reaches in a
/// session this host serves.
impl maho_core::session_control_actions::ControlEndpointHost for HostSessionControl {
    fn register(
        &self,
        options: maho_core::extensions::session_control_types::RegisterControlEndpointOptions,
    ) -> BoxFuture<'_, maho_core::extensions::session_control_types::SessionControlRegistration>
    {
        use maho_core::extensions::session_control_types::{
            ControlEndpointUnsupported, SessionControlRegistration,
        };
        Box::pin(async move {
            let drain = options.drain.clone();
            let drain: ControlDrain = Arc::new(move |event| {
                let drain = drain.clone();
                Box::pin(async move { drain(event).await })
            });
            match self
                .register(std::path::Path::new(&options.inbox_dir), drain)
                .await
            {
                ControlRegistration::Registered {
                    socket,
                    registration,
                } => {
                    let state = Arc::downgrade(&self.state);
                    SessionControlRegistration::Registered {
                        socket,
                        dispose: Arc::new(move || {
                            if let Some(state) = state.upgrade() {
                                Self::dispose_state(&state, registration);
                            }
                            Box::pin(async { Ok(()) })
                        }),
                    }
                }
                ControlRegistration::Unsupported { .. } => {
                    SessionControlRegistration::Unsupported {
                        reason: ControlEndpointUnsupported::UnsupportedMode,
                    }
                }
                ControlRegistration::Failed { reason } => {
                    SessionControlRegistration::Failed { reason }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSession {
        wakes: Mutex<Vec<Value>>,
    }
    impl ControlSession for FakeSession {
        fn emit_wake(&self, event: Value) -> BoxFuture<'static, ()> {
            self.wakes.lock().unwrap().push(event);
            Box::pin(async {})
        }
        fn on_idle(&self, _listener: Arc<dyn Fn() + Send + Sync>) -> EdgeSubscription {
            Box::new(())
        }
        fn on_emitted(&self, _listener: Arc<dyn Fn() + Send + Sync>) -> EdgeSubscription {
            Box::new(())
        }
    }

    fn control(
        socket: Option<&str>,
    ) -> (
        HostSessionControl,
        Arc<FakeSession>,
        Arc<Mutex<Vec<String>>>,
    ) {
        let session = Arc::new(FakeSession {
            wakes: Mutex::default(),
        });
        let reports = Arc::new(Mutex::new(Vec::new()));
        let sink = reports.clone();
        let control = HostSessionControl::new(
            session.clone(),
            socket.map(str::to_owned),
            Arc::new(move |line| sink.lock().unwrap().push(line)),
        );
        (control, session, reports)
    }

    /// senpi 1df92e349e: a repeating drain failure is reported once per distinct reason, again after a success.
    #[tokio::test]
    async fn a_repeating_drain_failure_is_reported_once_until_a_pass_succeeds() {
        let temp = tempfile::tempdir().unwrap();
        let (control, _session, reports) = control(Some("/tmp/host.sock"));
        let outcomes = Arc::new(Mutex::new(
            vec![
                Err("busy"),
                Err("busy"),
                Err("busy"),
                Ok(()),
                Err("busy"),
                Err("gone"),
            ]
            .into_iter(),
        ));
        let drain: ControlDrain = Arc::new(move |_| {
            let next = outcomes.lock().unwrap().next().unwrap_or(Ok(()));
            Box::pin(async move {
                next.map(|()| Some(json!({"admitted":["d1"]})))
                    .map_err(str::to_owned)
            })
        });
        let ControlRegistration::Registered { socket, .. } =
            control.register(&temp.path().join("inbox"), drain).await
        else {
            panic!("a host with a public socket registers");
        };
        assert_eq!(socket, "/tmp/host.sock");
        // The registration's first edge and the first wake share one pass (the pass had not started yet); every
        // later wake is one more pass, run to completion before the next is requested.
        for _ in 0..6 {
            control.wake(&[]).await;
        }
        assert_eq!(
            *reports.lock().unwrap(),
            [
                "control drain failed: busy",
                "control drain failed: busy",
                "control drain failed: gone"
            ]
        );
    }

    /// senpi host-session-control.ts: a registration runs one pass at once and one more once its inbox watch armed,
    /// so an entry written before the watch could see it is still drained.
    #[tokio::test]
    async fn a_registration_drains_at_once_and_again_once_its_watch_armed() {
        let temp = tempfile::tempdir().unwrap();
        let (control, _session, _) = control(Some("/tmp/host.sock"));
        let (started_tx, mut started) = tokio::sync::mpsc::unbounded_channel();
        let (release_tx, release) = tokio::sync::oneshot::channel::<()>();
        let release = Arc::new(Mutex::new(Some(release)));
        let drain: ControlDrain = Arc::new(move |event: Value| {
            let _ = started_tx.send(event["reason"].clone());
            let held = release.lock().unwrap().take();
            Box::pin(async move {
                // The first pass is held so the armed edge lands while it runs and merges into one more pass.
                if let Some(held) = held {
                    let _ = held.await;
                }
                Ok(None)
            })
        });
        assert!(matches!(
            control.register(&temp.path().join("inbox"), drain).await,
            ControlRegistration::Registered { .. }
        ));
        async fn next(started: &mut tokio::sync::mpsc::UnboundedReceiver<Value>) -> Value {
            tokio::time::timeout(std::time::Duration::from_secs(30), started.recv())
                .await.expect("drain starts").expect("drain channel open")
        }
        assert_eq!(next(&mut started).await, "inbox");
        release_tx.send(()).unwrap();
        assert_eq!(next(&mut started).await, "inbox");
        control.dispose();
    }

    #[test]
    fn registering_outside_a_runtime_fails_instead_of_panicking() {
        let temp = tempfile::tempdir().unwrap();
        let (control, _, _) = control(Some("/tmp/host.sock"));
        let ControlRegistration::Failed { reason } = futures::executor::block_on(control.register(
            &temp.path().join("inbox"),
            Arc::new(|_| Box::pin(async { Ok(None) })),
        )) else {
            panic!("a registration outside a runtime fails");
        };
        assert_eq!(reason, "no tokio runtime");
        assert!(control.state().active.is_none());
    }

    #[tokio::test]
    async fn stdio_hosts_refuse_and_an_unregistered_wake_still_emits() {
        let temp = tempfile::tempdir().unwrap();
        let (control, session, _) = control(None);
        assert!(matches!(
            control
                .register(temp.path(), Arc::new(|_| Box::pin(async { Ok(None) })))
                .await,
            ControlRegistration::Unsupported {
                reason: "unsupported_mode"
            }
        ));
        assert_eq!(control.wake(&["d1".into()]).await, json!({"admitted":[]}));
        assert_eq!(
            session.wakes.lock().unwrap()[0],
            json!({"type":"session_control_wake","reason":"command","reasons":["command"],"delivery_ids":["d1"]})
        );
    }
}
