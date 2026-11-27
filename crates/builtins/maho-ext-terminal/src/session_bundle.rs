use crate::{
    background_age::{BackgroundAgeSchedule, BackgroundAgeSubject, background_age_event},
    manager::TerminalManager,
    monitor_registry::{MonitorEndedEvent, MonitorEvent, MonitorRegistry, MonitorSnapshotEntry},
    tools::context::TerminalExitView,
};
use maho_ext_builtin_loose::monitor_state_event::{
    TerminalBackgroundAgeEvent, TerminalBackgroundAgeKind,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
type MonitorSink = Arc<dyn Fn(MonitorEvent) + Send + Sync>;
type EndedSink = Arc<dyn Fn(MonitorEndedEvent) + Send + Sync>;
type StateSink = Arc<dyn Fn(&[MonitorSnapshotEntry], bool) + Send + Sync>;
pub type BackgroundStateSink = Arc<dyn Fn(&[BackgroundSession]) + Send + Sync>;
pub type BackgroundExitSink = Arc<dyn Fn(&str, &TerminalExitView) + Send + Sync>;
pub type BackgroundAgeSink = Arc<dyn Fn(&TerminalBackgroundAgeEvent) + Send + Sync>;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BackgroundStartDetails {
    pub command: Option<String>,
    pub auto_reap_minutes: Option<u64>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundSession {
    pub id: String,
    pub description: String,
    pub started_at_ms: f64,
}
#[derive(Clone)]
pub struct TerminalEventSinks {
    pub on_monitor_event: MonitorSink,
    pub on_monitor_state: StateSink,
    pub on_monitor_ended: EndedSink,
    pub on_background_state: BackgroundStateSink,
    pub on_background_exit: BackgroundExitSink,
    pub on_background_age: Option<BackgroundAgeSink>,
}
#[derive(Default)]
struct Routing {
    sink: Option<MonitorSink>,
    parked: std::collections::VecDeque<MonitorEvent>,
    torndown: bool,
}
#[derive(Default)]
struct EndedRouting {
    sink: Option<EndedSink>,
    parked: std::collections::VecDeque<MonitorEndedEvent>,
}
#[derive(Default)]
struct AgeRouting {
    sink: Option<BackgroundAgeSink>,
    parked: indexmap::IndexMap<String, TerminalBackgroundAgeEvent>,
}
type RunJobRelease = Box<dyn FnOnce() + Send>;

#[derive(Clone)]
pub struct TerminalSessionBundle {
    pub manager: Arc<Mutex<TerminalManager>>,
    pub monitors: Arc<Mutex<MonitorRegistry>>,
    pub backgrounds: Arc<Mutex<indexmap::IndexMap<String, BackgroundSession>>>,
    routing: Arc<Mutex<Routing>>,
    ended_routing: Arc<Mutex<EndedRouting>>,
    age_routing: Arc<Mutex<AgeRouting>>,
    ages: BackgroundAgeSchedule,
    interrupt_stops: Arc<Mutex<BTreeMap<String, String>>>,
    run_job_releases: Arc<Mutex<BTreeMap<String, RunJobRelease>>>,
    parked_exits: Arc<Mutex<indexmap::IndexMap<String, Option<TerminalExitView>>>>,
    state_sink: Option<StateSink>,
    background_state: Option<BackgroundStateSink>,
    background_exit: Option<BackgroundExitSink>,
}
fn parked_bundles() -> &'static Mutex<std::collections::BTreeMap<String, TerminalSessionBundle>> {
    static BUNDLES: std::sync::OnceLock<
        Mutex<std::collections::BTreeMap<String, TerminalSessionBundle>>,
    > = std::sync::OnceLock::new();
    BUNDLES.get_or_init(Mutex::default)
}
pub fn park_bundle(
    session_key: &str,
    mut bundle: TerminalSessionBundle,
) -> Result<(), crate::runtime_session::RuntimeError> {
    bundle.park();
    let previous = parked_bundles()
        .lock()
        .map_err(|_| crate::runtime_session::RuntimeError::Poisoned)?
        .insert(session_key.to_owned(), bundle);
    // Re-parking the same generation (its arcs are shared with the bundle just stored) must not
    // tear down the live terminals; only a genuinely different stale bundle is torn down.
    if let Some(mut previous) = previous {
        let same = parked_bundles()
            .lock()
            .map_err(|_| crate::runtime_session::RuntimeError::Poisoned)?
            .get(session_key)
            .is_some_and(|stored| Arc::ptr_eq(&stored.manager, &previous.manager));
        if !same {
            previous.teardown()?;
        }
    }
    Ok(())
}
pub fn claim_parked_bundle(session_key: &str) -> Option<TerminalSessionBundle> {
    parked_bundles()
        .lock()
        .expect("parked bundles")
        .remove(session_key)
}
pub fn teardown_parked_bundle(
    session_key: &str,
) -> Result<(), crate::runtime_session::RuntimeError> {
    if let Some(mut bundle) = claim_parked_bundle(session_key) {
        bundle.teardown()?;
    }
    Ok(())
}
pub async fn teardown_parked_bundle_async(
    session_key: &str,
) -> Result<(), crate::runtime_session::RuntimeError> {
    if let Some(mut bundle) = claim_parked_bundle(session_key)
        && bundle.begin_teardown()?
    {
        crate::manager::teardown_shared(&bundle.manager).await?;
    }
    Ok(())
}
impl TerminalSessionBundle {
    pub fn new(max_sessions: usize) -> Self {
        let manager = Arc::new(Mutex::new(TerminalManager::new(max_sessions)));
        let backgrounds = Arc::new(Mutex::new(indexmap::IndexMap::new()));
        let routing = Arc::new(Mutex::new(Routing::default()));
        let dispatch = routing.clone();
        let ended_routing = Arc::new(Mutex::new(EndedRouting::default()));
        let ended_dispatch = ended_routing.clone();
        let age_routing = Arc::new(Mutex::new(AgeRouting::default()));
        let age_dispatch = age_routing.clone();
        let age_routing_gate = routing.clone();
        let ages = BackgroundAgeSchedule::new(move |event| {
            if age_routing_gate.lock().expect("terminal routing").torndown {
                return;
            }
            let sink = {
                let mut ar = age_dispatch.lock().expect("terminal age routing");
                if let Some(sink) = &ar.sink {
                    Some(sink.clone())
                } else {
                    ar.parked.insert(event.id.clone(), event.clone());
                    None
                }
            };
            if let Some(sink) = sink {
                sink(&event);
            }
        });
        let monitors = Arc::new(Mutex::new(MonitorRegistry::new_with_ended(
            move |event| {
                let sink = {
                    let mut routing = dispatch.lock().expect("terminal routing");
                    if routing.torndown {
                        return;
                    }
                    if let Some(sink) = &routing.sink {
                        Some(sink.clone())
                    } else {
                        routing.parked.push_back(event.clone());
                        if routing.parked.len() > 100 {
                            routing.parked.pop_front();
                        }
                        None
                    }
                };
                if let Some(sink) = sink {
                    sink(event);
                }
            },
            move |event| {
                let sink = {
                    let mut routing = ended_dispatch.lock().expect("terminal ended routing");
                    if let Some(sink) = &routing.sink {
                        Some(sink.clone())
                    } else {
                        routing.parked.push_back(event.clone());
                        if routing.parked.len() > 32 {
                            routing.parked.pop_front();
                        }
                        None
                    }
                };
                if let Some(sink) = sink {
                    sink(event);
                }
            },
        )));
        Self {
            manager,
            monitors,
            backgrounds,
            routing,
            ended_routing,
            age_routing,
            ages,
            interrupt_stops: Arc::new(Mutex::new(BTreeMap::new())),
            run_job_releases: Arc::new(Mutex::new(BTreeMap::new())),
            parked_exits: Arc::new(Mutex::new(indexmap::IndexMap::new())),
            state_sink: None,
            background_state: None,
            background_exit: None,
        }
    }
    pub fn from_parts(
        manager: Arc<Mutex<TerminalManager>>,
        monitors: Arc<Mutex<MonitorRegistry>>,
        backgrounds: Arc<Mutex<indexmap::IndexMap<String, BackgroundSession>>>,
    ) -> Self {
        let routing = Arc::new(Mutex::new(Routing::default()));
        let age_routing = Arc::new(Mutex::new(AgeRouting::default()));
        let age_dispatch = age_routing.clone();
        let age_routing_gate = routing.clone();
        let ages = BackgroundAgeSchedule::new(move |event| {
            if age_routing_gate.lock().expect("terminal routing").torndown {
                return;
            }
            let sink = {
                let mut ar = age_dispatch.lock().expect("terminal age routing");
                if let Some(sink) = &ar.sink {
                    Some(sink.clone())
                } else {
                    ar.parked.insert(event.id.clone(), event.clone());
                    None
                }
            };
            if let Some(sink) = sink {
                sink(&event);
            }
        });
        Self {
            manager,
            monitors,
            backgrounds,
            routing,
            ended_routing: Arc::new(Mutex::new(EndedRouting::default())),
            age_routing,
            ages,
            interrupt_stops: Arc::new(Mutex::new(BTreeMap::new())),
            run_job_releases: Arc::new(Mutex::new(BTreeMap::new())),
            parked_exits: Arc::new(Mutex::new(indexmap::IndexMap::new())),
            state_sink: None,
            background_state: None,
            background_exit: None,
        }
    }
    pub fn adopt(&self, source: &TerminalSessionBundle) {
        // senpi claims the parked bundle as-is (`state.bundle = claimed`). When the parked bundle
        // shares this generation's arcs, taking from it would swap in empty state and drop the
        // live manager, killing every running terminal and monitor process group.
        if Arc::ptr_eq(&self.manager, &source.manager) {
            return;
        }
        *self
            .manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = std::mem::take(
            &mut *source
                .manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        *self
            .monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = std::mem::replace(
            &mut *source
                .monitors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            MonitorRegistry::new(|_| {}),
        );
        *self.backgrounds.lock().expect("terminal backgrounds") =
            std::mem::take(&mut *source.backgrounds.lock().expect("terminal backgrounds"));
    }
    pub fn bind(&mut self, sink: MonitorSink) {
        let events = {
            let mut routing = self.routing.lock().expect("terminal routing");
            if routing.torndown {
                return;
            }
            routing.sink = Some(sink.clone());
            std::mem::take(&mut routing.parked)
        };
        for event in events {
            sink(event);
        }
    }
    pub fn bind_sinks(&mut self, sinks: TerminalEventSinks) {
        self.bind_sinks_inner(sinks, true);
    }

    /// Bind sinks during extension registration without immediately emitting empty initial
    /// monitor/background state snapshots (still flushes any parked events/exits/ages).
    pub fn bind_sinks_quiet(&mut self, sinks: TerminalEventSinks) {
        self.bind_sinks_inner(sinks, false);
    }

    fn bind_sinks_inner(&mut self, sinks: TerminalEventSinks, emit_initial_state: bool) {
        if self.routing.lock().expect("terminal routing").torndown {
            return;
        }
        self.bind(sinks.on_monitor_event.clone());
        {
            let mut routing = self.ended_routing.lock().expect("terminal ended routing");
            routing.sink = Some(sinks.on_monitor_ended.clone());
        }
        self.state_sink = Some(sinks.on_monitor_state.clone());
        self.background_state = Some(sinks.on_background_state.clone());
        self.background_exit = Some(sinks.on_background_exit.clone());
        if emit_initial_state {
            (sinks.on_background_state)(&self.background_snapshot());
        }
        let parked = std::mem::take(&mut *self.parked_exits.lock().expect("parked exits"));
        for (id, stored_view) in parked {
            let view = stored_view.or_else(|| {
                self.manager
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&id)
                    .map(|runtime| TerminalExitView::of_runtime(runtime))
            });
            if let Some(view) = view {
                (sinks.on_background_exit)(&id, &view);
            }
        }
        let parked_ages = {
            let mut ar = self.age_routing.lock().expect("terminal age routing");
            ar.sink = sinks.on_background_age.clone();
            ar.parked
                .drain(..)
                .map(|(_, event)| event)
                .collect::<Vec<_>>()
        };
        if let Some(age_sink) = &sinks.on_background_age {
            for event in parked_ages {
                age_sink(&event);
            }
        }
        let events = std::mem::take(
            &mut self
                .ended_routing
                .lock()
                .expect("terminal ended routing")
                .parked,
        );
        for event in events {
            (sinks.on_monitor_ended)(event);
        }
    }
    pub fn bind_backgrounds(&mut self, state: BackgroundStateSink, exit: BackgroundExitSink) {
        if self.routing.lock().expect("terminal routing").torndown {
            return;
        }
        self.background_state = Some(state.clone());
        self.background_exit = Some(exit.clone());
        state(&self.background_snapshot());
        let parked = std::mem::take(&mut *self.parked_exits.lock().expect("parked exits"));
        for (id, stored_view) in parked {
            let view = stored_view.or_else(|| {
                self.manager
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&id)
                    .map(|runtime| TerminalExitView::of_runtime(runtime))
            });
            if let Some(view) = view {
                exit(&id, &view);
            }
        }
    }
    pub fn publish_state(&self, snapshot: &[MonitorSnapshotEntry], transition: bool) {
        if let Some(sink) = &self.state_sink {
            sink(snapshot, transition);
        }
    }
    pub fn background_snapshot(&self) -> Vec<BackgroundSession> {
        self.backgrounds
            .lock()
            .expect("terminal backgrounds")
            .values()
            .cloned()
            .collect()
    }
    pub fn notify_background_start(&mut self, id: &str, description: &str, started_at_ms: f64) {
        self.notify_background_start_with_details(
            id,
            description,
            started_at_ms,
            BackgroundStartDetails::default(),
        );
    }
    pub fn notify_background_start_with_details(
        &mut self,
        id: &str,
        description: &str,
        started_at_ms: f64,
        details: BackgroundStartDetails,
    ) {
        if self.routing.lock().expect("terminal routing").torndown {
            return;
        }
        self.backgrounds
            .lock()
            .expect("terminal backgrounds")
            .insert(
                id.to_owned(),
                BackgroundSession {
                    id: id.to_owned(),
                    description: description.to_owned(),
                    started_at_ms,
                },
            );
        self.ages.add(BackgroundAgeSubject {
            id: id.to_owned(),
            description: description.to_owned(),
            command: details.command,
            started_at_ms,
            auto_reap_minutes: details.auto_reap_minutes.map(|m| m as f64),
        });
        if let Some(state) = &self.background_state {
            state(&self.background_snapshot());
        }
    }
    /// Hold the release of the run job `id` was recorded as; its exit drops it.
    pub fn track_run_job(&self, id: &str, release: impl FnOnce() + Send + 'static) {
        if self.routing.lock().expect("terminal routing").torndown {
            release();
            return;
        }
        self.run_job_releases
            .lock()
            .expect("run job releases")
            .insert(id.to_owned(), Box::new(release));
    }
    /// Stop a session the interrupted turn `turn_id` started; its exit notice then reaches
    /// the next turn without waking.
    pub async fn stop_for_interrupt(&self, id: &str, turn_id: &str) -> Result<(), String> {
        let mut exit_rx = {
            let mut manager = self
                .manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(runtime) = manager.get(id) else {
                return Err(format!(
                    "{id} runs on the session's tool child, which this stop does not reach"
                ));
            };
            runtime.subscribe_exit()
        };
        self.interrupt_stops
            .lock()
            .expect("interrupt stops")
            .insert(id.to_owned(), turn_id.to_owned());
        let stopped = match crate::manager::stop_shared(&self.manager, id).await {
            Ok(stopped) => stopped,
            Err(error) => {
                let mut stops = self.interrupt_stops.lock().expect("interrupt stops");
                if stops.get(id).is_some_and(|turn| turn == turn_id) {
                    stops.remove(id);
                }
                return Err(error.to_string());
            }
        };
        let exited = exit_rx.borrow_and_update().is_some()
            || self
                .manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(id)
                .is_none_or(|rt| rt.exited().unwrap_or(false));
        if stopped && exited {
            return Ok(());
        }
        self.interrupt_stops
            .lock()
            .expect("interrupt stops")
            .remove(id);
        if !stopped {
            return Err(format!("the terminal manager has no session {id} to stop"));
        }
        Err(format!(
            "{id} was still running after the stop (TERM, then KILL)"
        ))
    }
    fn dispatch_age(&self, event: TerminalBackgroundAgeEvent) {
        if self.routing.lock().expect("terminal routing").torndown {
            return;
        }
        let sink = {
            let mut ar = self.age_routing.lock().expect("terminal age routing");
            if let Some(sink) = &ar.sink {
                Some(sink.clone())
            } else {
                ar.parked.insert(event.id.clone(), event.clone());
                None
            }
        };
        if let Some(sink) = sink {
            sink(&event);
        }
    }
    /// The auto-reap's stop did not end a session still on the schedule: one notice; its age notices go on.
    pub fn notify_background_reap_failed(&self, id: &str, auto_reap_minutes: u64, reason: &str) {
        if self.routing.lock().expect("terminal routing").torndown {
            return;
        }
        let Some(mut subject) = self.ages.get(id) else {
            return;
        };
        subject.auto_reap_minutes = Some(auto_reap_minutes as f64);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        let mut event =
            background_age_event(TerminalBackgroundAgeKind::ReapFailed, &subject, now_ms);
        event.reason = Some(reason.to_owned());
        self.dispatch_age(event);
    }
    pub fn notify_background_exit(&mut self, id: &str) {
        self.notify_background_exit_with_view(id, None);
    }
    pub fn notify_background_exit_with_view(&mut self, id: &str, exited: Option<TerminalExitView>) {
        if self.routing.lock().expect("terminal routing").torndown {
            return;
        }
        let release = self
            .run_job_releases
            .lock()
            .expect("run job releases")
            .remove(id);
        if let Some(release) = release {
            release();
        }
        let interrupted_turn = self
            .interrupt_stops
            .lock()
            .expect("interrupt stops")
            .remove(id);
        let subject = self.ages.remove(id);
        self.age_routing
            .lock()
            .expect("terminal age routing")
            .parked
            .shift_remove(id);
        let mut view = exited.or_else(|| {
            self.manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(id)
                .map(|runtime| TerminalExitView::of_runtime(runtime))
        });
        if let (Some(turn_id), Some(v)) = (interrupted_turn, view.as_mut()) {
            *v = v.clone().with_stopped_by_interrupt(turn_id);
        }
        if let (Some(mut subj), Some(minutes)) = (
            subject,
            view.as_ref().and_then(|v| v.auto_reaped_after_minutes),
        ) {
            subj.auto_reap_minutes = Some(minutes as f64);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            self.dispatch_age(background_age_event(
                TerminalBackgroundAgeKind::Reaped,
                &subj,
                now_ms,
            ));
        }
        let removed = self
            .backgrounds
            .lock()
            .expect("terminal backgrounds")
            .shift_remove(id)
            .is_some();
        if removed && let Some(state) = &self.background_state {
            state(&self.background_snapshot());
        }
        if let Some(exit) = &self.background_exit {
            if let Some(view) = view {
                exit(id, &view);
            }
            return;
        }
        let mut parked = self.parked_exits.lock().expect("parked exits");
        if parked.len() < 32 {
            parked.insert(id.to_owned(), view);
        }
    }
    pub fn park(&mut self) {
        self.routing.lock().expect("terminal routing").sink = None;
        self.ended_routing
            .lock()
            .expect("terminal ended routing")
            .sink = None;
        self.age_routing.lock().expect("terminal age routing").sink = None;
        self.state_sink = None;
        self.background_state = None;
        self.background_exit = None;
    }
    pub fn teardown(&mut self) -> Result<(), crate::runtime_session::RuntimeError> {
        if !self.begin_teardown()? {
            return Ok(());
        }
        self.manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .teardown()
    }
    /// Everything teardown does before stopping terminals; `false` when already torn down. Async
    /// callers then await `manager::teardown_shared` on `manager` with no bundle lock held, so the
    /// runtime thread is not blocked (upstream awaits `manager.teardown()`).
    pub fn begin_teardown(&mut self) -> Result<bool, crate::runtime_session::RuntimeError> {
        {
            let mut routing = self.routing.lock().expect("terminal routing");
            if routing.torndown {
                return Ok(false);
            }
            routing.torndown = true;
            routing.sink = None;
            routing.parked.clear();
        }
        self.ended_routing
            .lock()
            .expect("terminal ended routing")
            .parked
            .clear();
        self.ages.dispose();
        {
            let mut ar = self.age_routing.lock().expect("terminal age routing");
            ar.sink = None;
            ar.parked.clear();
        }
        let releases =
            std::mem::take(&mut *self.run_job_releases.lock().expect("run job releases"));
        for (_, release) in releases {
            release();
        }
        self.interrupt_stops
            .lock()
            .expect("interrupt stops")
            .clear();
        self.parked_exits.lock().expect("parked exits").clear();
        let had_backgrounds = !self
            .backgrounds
            .lock()
            .expect("terminal backgrounds")
            .is_empty();
        if had_backgrounds {
            self.backgrounds
                .lock()
                .expect("terminal backgrounds")
                .clear();
            if let Some(state) = &self.background_state {
                state(&[]);
            }
        }
        self.state_sink = None;
        self.background_state = None;
        self.background_exit = None;
        self.monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .dispose();
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_job_release_callback_runs_without_the_release_lock() {
        let mut bundle = TerminalSessionBundle::new(1);
        let releases = Arc::clone(&bundle.run_job_releases);
        let (sender, receiver) = std::sync::mpsc::channel();
        bundle.track_run_job("job", move || {
            sender.send(releases.try_lock().is_ok()).unwrap();
        });
        bundle.notify_background_exit("job");
        assert!(receiver.try_recv().unwrap());
        bundle.teardown().unwrap();
    }

    #[tokio::test]
    async fn interrupt_stop_error_clears_the_interrupt_marker() {
        let mut bundle = TerminalSessionBundle::new(1);
        let manager =
            crate::manager::TerminalManager::new(1).with_kill_process(Arc::new(|_, _| {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            }));
        bundle.manager = Arc::new(Mutex::new(manager));
        let id = {
            let mut manager = bundle.manager.lock().expect("manager");
            let id = manager
                .create("read", crate::shared::test_sh_options("read value"))
                .expect("create terminal");
            manager.track_detached_child(
                &id,
                crate::manager::TrackedDetachedChild {
                    process_group_id: Some(12345),
                    ..Default::default()
                },
            );
            id
        };
        bundle
            .stop_for_interrupt(&id, "turn")
            .await
            .expect_err("detached child cleanup fails");
        assert!(
            !bundle
                .interrupt_stops
                .lock()
                .expect("interrupt stops")
                .contains_key(&id)
        );
        bundle.teardown().expect("bundle teardown");
    }

    #[test]
    fn parked_background_exit_republishes_state_and_flushes_to_new_owner() {
        let mut bundle = TerminalSessionBundle::new(2);
        let id = bundle
            .manager
            .lock()
            .unwrap()
            .create(
                "ready",
                crate::shared::test_sh_options("stty -echo; printf 'ready\\n'"),
            )
            .unwrap();
        bundle
            .manager
            .lock()
            .unwrap()
            .get(&id)
            .unwrap()
            .wait(std::time::Duration::from_secs(5))
            .unwrap();
        bundle.notify_background_start(&id, "command", 1.0);
        assert_eq!(bundle.background_snapshot()[0].id, id);
        bundle.park();
        bundle.notify_background_exit(&id);
        assert!(bundle.background_snapshot().is_empty());
        let (sender, receiver) = std::sync::mpsc::channel();
        let state_sender = sender.clone();
        bundle.bind_backgrounds(
            Arc::new(move |state| {
                state_sender.send(format!("state {}", state.len())).unwrap();
            }),
            Arc::new(move |id, view| {
                sender
                    .send(format!("exit {id} {}", view.full_output().trim()))
                    .unwrap();
            }),
        );
        assert_eq!(receiver.try_recv().unwrap(), "state 0");
        assert_eq!(receiver.try_recv().unwrap(), format!("exit {id} ready"));
        assert!(receiver.try_recv().is_err());
        assert!(bundle.parked_exits.lock().unwrap().is_empty());
        bundle.teardown().unwrap();
    }
    #[test]
    fn claim_transfers_the_same_bundle_only_once() {
        let bundle = TerminalSessionBundle::new(1);
        let manager = bundle.manager.clone();
        park_bundle("bundle-claim-test", bundle).unwrap();
        let mut claimed = claim_parked_bundle("bundle-claim-test").expect("parked bundle");
        assert!(Arc::ptr_eq(&claimed.manager, &manager));
        assert!(claim_parked_bundle("bundle-claim-test").is_none());
        claimed.teardown().unwrap();
    }
    #[test]
    fn adopt_moves_live_state_between_generation_arcs() {
        let mut source = TerminalSessionBundle::new(1);
        let id = source
            .manager
            .lock()
            .unwrap()
            .create(
                "ready",
                crate::shared::test_sh_options("stty -echo; printf 'ready\\n'"),
            )
            .unwrap();
        source.notify_background_start(&id, "command", 1.0);
        let mut target = TerminalSessionBundle::new(4);
        target.adopt(&source);
        assert_eq!(target.manager.lock().unwrap().size(), 1);
        assert_eq!(target.background_snapshot()[0].id, id);
        assert_eq!(source.manager.lock().unwrap().size(), 0);
        assert!(source.background_snapshot().is_empty());
        target.teardown().unwrap();
    }
    #[test]
    fn adopt_same_bundle_keeps_live_sessions() {
        let mut live = TerminalSessionBundle::new(2);
        let id = live
            .manager
            .lock()
            .unwrap()
            .create("sleeper", crate::shared::test_sh_options("read line"))
            .unwrap();
        let parked = live.clone();
        live.adopt(&parked);
        let mut manager = live.manager.lock().unwrap();
        assert_eq!(manager.size(), 1);
        assert!(!manager.get(&id).unwrap().exited().unwrap());
        drop(manager);
        live.teardown().unwrap();
    }
    #[test]
    fn park_same_bundle_twice_keeps_live_sessions() {
        let live = TerminalSessionBundle::new(2);
        let id = live
            .manager
            .lock()
            .unwrap()
            .create("sleeper", crate::shared::test_sh_options("read line"))
            .unwrap();
        park_bundle("park-twice-test", live.clone()).unwrap();
        park_bundle("park-twice-test", live.clone()).unwrap();
        assert!(
            !live
                .manager
                .lock()
                .unwrap()
                .get(&id)
                .unwrap()
                .exited()
                .unwrap()
        );
        let mut claimed = claim_parked_bundle("park-twice-test").expect("parked bundle");
        claimed.teardown().unwrap();
    }
    #[tokio::test]
    async fn parked_monitor_completion_flushes_into_new_owner() {
        let mut bundle = TerminalSessionBundle::new(1);
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "ready",
            crate::shared::test_sh_options("stty -echo; printf 'ready\\n'"),
        )
        .unwrap();
        runtime.wait(std::time::Duration::from_secs(5)).unwrap();
        bundle
            .monitors
            .lock()
            .unwrap()
            .register(
                &runtime,
                crate::monitor_registry::CommandMonitor::new(
                    crate::monitor_registry::MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        ..Default::default()
                    },
                    None,
                ),
            )
            .unwrap();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        bundle.bind(Arc::new(move |event| {
            sender.send(event).unwrap();
        }));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            assert!(matches!(
                events.recv().await,
                Some(MonitorEvent::Line { .. })
            ));
            assert!(matches!(
                events.recv().await,
                Some(MonitorEvent::Summary { .. })
            ));
        })
        .await
        .unwrap();
        bundle.teardown().unwrap();
        runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn parked_monitor_ending_flushes_to_new_owner_and_publishes_state() {
        let mut bundle = TerminalSessionBundle::new(1);
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "ready",
            crate::shared::test_sh_options("stty -echo; printf 'ready\\n'"),
        )
        .unwrap();
        runtime.wait(std::time::Duration::from_secs(5)).unwrap();
        let (ended_sender, mut ended) = tokio::sync::mpsc::unbounded_channel();
        let (state_sender, mut states) = tokio::sync::mpsc::unbounded_channel();
        bundle.park();
        bundle
            .monitors
            .lock()
            .unwrap()
            .register(
                &runtime,
                crate::monitor_registry::CommandMonitor::new(
                    crate::monitor_registry::MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        description: "ready".to_owned(),
                        started_at_ms: 5.0,
                        ..Default::default()
                    },
                    None,
                ),
            )
            .unwrap();
        let mut registry_state = bundle.monitors.lock().unwrap().subscribe_state();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if registry_state.borrow_and_update().is_empty() {
                    break;
                }
                if registry_state.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let sinks = TerminalEventSinks {
            on_monitor_event: Arc::new(|_| {}),
            on_monitor_state: Arc::new(move |_, transition| {
                state_sender.send(transition).unwrap();
            }),
            on_monitor_ended: Arc::new(move |event| {
                ended_sender.send(event).unwrap();
            }),
            on_background_state: Arc::new(|_| {}),
            on_background_exit: Arc::new(|_, _| {}),
            on_background_age: None,
        };
        bundle.bind_sinks(sinks);
        let event = ended.try_recv().expect("flushed ending");
        assert_eq!(event.id, "bash_1");
        assert_eq!(
            (event.reason, event.exit_code),
            (crate::monitor_registry::MonitorEndedReason::Exit, Some(0))
        );
        assert!(event.fire_count >= 1);
        assert!(states.try_recv().is_err());
        bundle.publish_state(&bundle.monitors.lock().unwrap().snapshot(), false);
        assert!(!states.try_recv().unwrap());
        bundle.teardown().unwrap();
        runtime.dispose().unwrap();
    }

    #[tokio::test]
    async fn interrupt_stop_marks_exit_view_and_dispatches_reap_failed_and_reaped_ages() {
        let mut bundle = TerminalSessionBundle::new(2);
        let id = bundle
            .manager
            .lock()
            .unwrap()
            .create("sleeper", crate::shared::test_sh_options("sleep 60"))
            .unwrap();
        let (age_tx, mut age_rx) = tokio::sync::mpsc::unbounded_channel();
        let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();
        let sinks = TerminalEventSinks {
            on_monitor_event: Arc::new(|_| {}),
            on_monitor_state: Arc::new(|_, _| {}),
            on_monitor_ended: Arc::new(|_| {}),
            on_background_state: Arc::new(|_| {}),
            on_background_exit: Arc::new(move |id, view| {
                let _ = exit_tx.send((
                    id.to_owned(),
                    view.stopped_by_interrupt.clone(),
                    view.auto_reaped_after_minutes,
                ));
            }),
            on_background_age: Some(Arc::new(move |event| {
                let _ = age_tx.send(event.clone());
            })),
        };
        bundle.bind_sinks(sinks);
        bundle.notify_background_start_with_details(
            &id,
            "sleeper",
            1_000.0,
            BackgroundStartDetails {
                command: Some("sleep 60".to_owned()),
                auto_reap_minutes: Some(5),
            },
        );
        bundle.notify_background_reap_failed(&id, 5, "EPERM");
        let failed = age_rx.try_recv().expect("reap_failed age");
        assert_eq!(failed.kind, TerminalBackgroundAgeKind::ReapFailed);
        assert_eq!(failed.reason.as_deref(), Some("EPERM"));

        bundle
            .stop_for_interrupt(&id, "turn_42")
            .await
            .expect("stop_for_interrupt");
        let mut view =
            TerminalExitView::of_runtime(bundle.manager.lock().unwrap().get(&id).expect("runtime"));
        view = view.with_auto_reaped_after_minutes(5);
        bundle.notify_background_exit_with_view(&id, Some(view));

        let reaped = age_rx.try_recv().expect("reaped age");
        assert_eq!(reaped.kind, TerminalBackgroundAgeKind::Reaped);
        let (exited_id, turn_id, reaped_mins) = exit_rx.try_recv().expect("exit");
        assert_eq!(exited_id, id);
        assert_eq!(turn_id.as_deref(), Some("turn_42"));
        assert_eq!(reaped_mins, Some(5));
        bundle.teardown().unwrap();
    }
}
