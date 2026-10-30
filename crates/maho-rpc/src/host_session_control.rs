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
use crate::session_control_wake::{DrainResult, WakeScheduler, empty_result};
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
    inbox: Option<notify::RecommendedWatcher>,
}

impl Active {
    fn release(self) {
        drop(self.inbox);
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
    state: Mutex<State>,
}

/// senpi `INBOX_ENGINE_ENTRY_PREFIX`: the engine's own inbox entries never count as deliveries.
pub const INBOX_ENGINE_ENTRY_PREFIX: &str = ".senpi-";

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
            state: Mutex::default(),
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
    pub fn register(
        &self,
        inbox_dir: &std::path::Path,
        drain: ControlDrain,
    ) -> ControlRegistration {
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
        let inbox = match watch_inbox(inbox_dir, wake("inbox"), {
            let report = self.report.clone();
            Arc::new(move |error: String| report(format!("control inbox watch failed: {error}")))
        }) {
            Ok(inbox) => inbox,
            Err(error) => {
                drop((idle, emitted));
                scheduler.dispose();
                return ControlRegistration::Failed {
                    reason: error.to_string(),
                };
            }
        };
        let mut state = self.state();
        // A rebind may dispose this host while the watch arms; a registration it outlived stays inert.
        if state.disposed {
            drop(state);
            drop(inbox);
            scheduler.dispose();
            return ControlRegistration::Unsupported {
                reason: "unsupported_mode",
            };
        }
        state.next_id += 1;
        let id = state.next_id;
        if let Some(previous) = state.active.replace(Active {
            id,
            scheduler: scheduler.clone(),
            _idle: idle,
            _emitted: emitted,
            inbox: Some(inbox),
        }) {
            previous.release();
        }
        drop(state);
        // Anything that reached the inbox before the watch was armed is picked up by this first pass. ADAPTATION:
        // `notify` arms synchronously, so senpi's second pass on `armed` is the same edge and is not repeated.
        drop(scheduler.request("inbox", &[]));
        ControlRegistration::Registered {
            socket,
            registration: id,
        }
    }

    /// The registration's `dispose`: releases it only while it is still the active one.
    pub fn dispose_registration(&self, registration: u64) {
        let released = {
            let mut state = self.state();
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

/// One watch on the inbox directory (senpi `watchInbox`): a created or removed entry that is not the engine's own
/// wakes the drain.
fn watch_inbox(
    inbox_dir: &std::path::Path,
    on_change: Arc<dyn Fn() + Send + Sync>,
    on_error: Arc<dyn Fn(String) + Send + Sync>,
) -> std::io::Result<notify::RecommendedWatcher> {
    use notify::Watcher;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(inbox_dir)?;
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event) => {
                let engine_only = !event.paths.is_empty()
                    && event.paths.iter().all(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(INBOX_ENGINE_ENTRY_PREFIX))
                    });
                if !engine_only {
                    on_change();
                }
            }
            Err(error) => on_error(error.to_string()),
        })
        .map_err(std::io::Error::other)?;
    watcher
        .watch(inbox_dir, notify::RecursiveMode::NonRecursive)
        .map_err(std::io::Error::other)?;
    Ok(watcher)
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
            control.register(&temp.path().join("inbox"), drain)
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

    #[tokio::test]
    async fn stdio_hosts_refuse_and_an_unregistered_wake_still_emits() {
        let temp = tempfile::tempdir().unwrap();
        let (control, session, _) = control(None);
        assert!(matches!(
            control.register(temp.path(), Arc::new(|_| Box::pin(async { Ok(None) }))),
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
