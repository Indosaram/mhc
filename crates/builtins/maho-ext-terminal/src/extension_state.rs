//! Port of extension-state.ts: the terminal extension's per-generation state, the bundle event
//! sinks it binds, and the tool context over it.
use crate::monitor_registry::{
    MonitorEndedEvent, MonitorEndedReason, MonitorEvent, MonitorSnapshotEntry,
};
use crate::session_bundle::{BackgroundSession, TerminalEventSinks, TerminalSessionBundle};
use crate::tools::context::{TerminalConfiguration, TerminalExitView, TerminalToolContext};
use maho_ext_api::types::{ExtensionApi, ExtensionContext};
use maho_tools::definition::ToolContent;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The live background sessions, in start order (upstream `bundle.backgrounds`).
pub type Backgrounds = Arc<Mutex<indexmap::IndexMap<String, BackgroundSession>>>;
/// The session's manifest recorder (upstream `state.manifestWriter`), bound while a lifecycle owns it.
pub type Lifecycle = Arc<tokio::sync::Mutex<Option<crate::terminal_state::TerminalStateLifecycle>>>;

/// Upstream `state.recordedBackgroundIds`: the background ids already recorded into the current
/// manifest writer; cleared whenever the writer is swapped or detached.
pub type RecordedBackgroundIds = Arc<Mutex<BTreeSet<String>>>;

/// One lazy persistence binding (upstream `state.ensurePersistence`): a session with no manifest on
/// disk takes no lease and binds no recorder at start; the first durable need (a monitor create, a
/// background session) runs `bind` (acquire or wait, then the restore in flight) exactly once.
#[derive(Clone)]
pub struct LazyPersistence {
    once: Arc<tokio::sync::OnceCell<()>>,
    bind: Arc<dyn Fn() -> PersistenceFuture + Send + Sync>,
}
pub type PersistenceFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
/// The current generation's lazy binding; `None` once persistence is bound eagerly or detached.
pub type EnsurePersistence = Arc<Mutex<Option<LazyPersistence>>>;

impl LazyPersistence {
    pub fn new(bind: impl Fn() -> PersistenceFuture + Send + Sync + 'static) -> Self {
        Self {
            once: Arc::default(),
            bind: Arc::new(bind),
        }
    }
}

/// Upstream `ctx.ensurePersistence()`: bind persistence once and await it (the binding is shared,
/// so concurrent callers await the same acquire). A no-op without a lazy binding. No std lock is
/// held across the await.
pub async fn ensure_persistence(slot: &EnsurePersistence) {
    let lazy = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(lazy) = lazy {
        lazy.once.get_or_init(|| (lazy.bind)()).await;
    }
}

/// Install `next` as the manifest recorder (or detach it with `None`) and hand back the previous
/// one. Upstream clears `recordedBackgroundIds` at every writer swap (the reload rebind, `own`,
/// `detachPersistence`), so the next background state re-records every live session into the new
/// writer.
pub fn replace_lifecycle(
    slot: &mut Option<crate::terminal_state::TerminalStateLifecycle>,
    recorded: &RecordedBackgroundIds,
    next: Option<crate::terminal_state::TerminalStateLifecycle>,
) -> Option<crate::terminal_state::TerminalStateLifecycle> {
    recorded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    std::mem::replace(slot, next)
}

/// Upstream `TerminalExtensionState`, the part the sinks and the tool context read at call time:
/// every field is shared, so a notifier or context swap needs no re-bind.
#[derive(Clone)]
pub struct TerminalExtensionState {
    pub sender: Arc<ExtensionApi>,
    /// `state.ctx`, set by the first session-start handler.
    pub ctx: Arc<Mutex<Option<ExtensionContext>>>,
    /// `state.settings` and `state.shellPath`.
    pub configuration: TerminalConfiguration,
    /// `state.ctx?.cwd`, set by the first session-start handler.
    pub session_cwd: Arc<Mutex<Option<PathBuf>>>,
    pub backgrounds: Backgrounds,
    pub lifecycle: Lifecycle,
    pub monitor_notifier: Arc<Mutex<Option<crate::monitor_notify::MonitorNotifier>>>,
    pub notifier: Arc<Mutex<crate::notify::TerminalNotifier>>,
    /// The completion delivery the notifier reads (`getMode` resolved for the session's mode).
    pub delivery: Arc<Mutex<Option<crate::notify::NotificationDelivery>>>,
    pub recorded_background_ids: RecordedBackgroundIds,
    /// Upstream `state.ensurePersistence`.
    pub ensure_persistence: EnsurePersistence,
}

pub fn manifest_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

/// Upstream `sessionKeyOf`: the session id, or `None` for a context that carries none.
pub fn session_key_of(ctx: Option<&ExtensionContext>) -> Option<String> {
    let id = ctx?.session_manager.session_id();
    (!id.is_empty()).then(|| id.to_owned())
}

/// Upstream `createBundle`: a fresh bundle sized and configured by the session settings.
pub fn create_bundle(
    settings: &crate::settings::ResolvedTerminalSettings,
) -> TerminalSessionBundle {
    let bundle = TerminalSessionBundle::new(settings.max_sessions as usize);
    bundle
        .manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .configure(settings);
    bundle
}

pub fn publish_backgrounds(backgrounds: &Backgrounds, sender: &ExtensionApi) {
    let snapshot = backgrounds
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .cloned()
        .collect::<Vec<_>>();
    emit_background_state(&snapshot, sender);
}

fn emit_background_state(snapshot: &[BackgroundSession], sender: &ExtensionApi) {
    let items = snapshot
        .iter()
        .map(|entry| json!({"id":entry.id,"description":entry.description,"startedAtMs":entry.started_at_ms}))
        .collect::<Vec<_>>();
    sender.events.emit(
        crate::shared::WAKE_SOURCE_STATE_EVENT,
        &json!({"source":"terminal-background-sessions","activeCount":items.len(),"items":items}),
    );
}

/// `state.notifier?.notifyCompletion(id, runtime)`: the once-only completion notice of a background session, read
/// through `TerminalExitView` (a live session's, or the summary a tool child sent).
pub fn send_completion_notice(
    id: &str,
    view: &TerminalExitView,
    sender: &Arc<ExtensionApi>,
    delivery: &Arc<Mutex<Option<crate::notify::NotificationDelivery>>>,
    notifier: &Arc<Mutex<crate::notify::TerminalNotifier>>,
) {
    notifier
        .lock()
        .expect("terminal notifier")
        .notify_completion(
            id,
            *delivery.lock().expect("terminal delivery"),
            view,
            |content, delivery| {
                use maho_ext_api::types::{CustomMessage, DeliverAs, SendMessageOptions};
                sender
                    .send_message(
                        CustomMessage {
                            custom_type: crate::notify::TERMINAL_NOTIFICATION_CUSTOM_TYPE
                                .to_owned(),
                            content: vec![ToolContent::text(content)],
                            display: false,
                            details: None,
                        },
                        SendMessageOptions {
                            trigger_turn: true,
                            deliver_as: Some(
                                if delivery == crate::notify::NotificationDelivery::Steer {
                                    DeliverAs::Steer
                                } else {
                                    DeliverAs::FollowUp
                                },
                            ),
                        },
                    )
                    .map_err(|error| {
                        maho_ext_api::diagnostic(&format!("terminal notification failed: {error}"));
                        error.to_string()
                    })?;
                Ok(())
            },
        );
}

pub fn monitor_state_payload(snapshot: &[MonitorSnapshotEntry]) -> Value {
    let monitors=snapshot.iter().map(|entry| {
        let mut value=json!({"id":entry.id,"description":entry.description,"paused":entry.paused,"startedAtMs":entry.started_at_ms,"command":entry.command,"filter":entry.filter,"deadlineMs":entry.deadline_ms,"lastFiredAtMs":entry.last_fired_at_ms});
        for (key,field) in [("command",entry.command.as_ref().map(|value|json!(value))),("filter",entry.filter.as_ref().map(|value|json!(value))),("persistent",entry.persistent.map(|value|json!(value))),("deadlineMs",entry.deadline_ms.map(|value|json!(value))),("fireCount",entry.fire_count.map(|value|json!(value))),("lastFiredAtMs",entry.last_fired_at_ms.map(|value|json!(value)))] {if let Some(field)=field {value[key]=field;}}
        value
    }).collect::<Vec<_>>();
    json!({"activeCount":snapshot.len(),"monitors":monitors})
}

/// `onMonitorState` minus the manifest sync: the state event on the bus and over RPC, and on a
/// transition the wake-source state, naming each watch by its stable `mon_` id because runtime ids
/// churn on restore and item ids feed the custody signature.
pub fn emit_monitor_state(
    sender: &ExtensionApi,
    snapshot: &[MonitorSnapshotEntry],
    transition: bool,
) {
    let payload = monitor_state_payload(snapshot);
    sender
        .events
        .emit(crate::shared::TERMINAL_MONITOR_STATE_EVENT, &payload);
    if let Err(error) = sender.rpc_emit(crate::shared::TERMINAL_MONITOR_STATE_EVENT, &payload) {
        maho_ext_api::diagnostic(&format!("monitor state RPC delivery failed: {error}"));
    }
    if !transition {
        return;
    }
    sender.events.emit(crate::shared::WAKE_SOURCE_STATE_EVENT,&json!({"source":"terminal-monitors","activeCount":snapshot.len(),"monitors":snapshot.iter().map(|entry|json!({"id":entry.monitor_id.as_ref().unwrap_or(&entry.id),"description":entry.description,"startedAtMs":entry.started_at_ms})).collect::<Vec<_>>()}));
}

/// Whether a fire-window change is fire bookkeeping: a fire only grows the window count, or rolls
/// the window over (a new `start_ms`) while the fire count grows. A count that drops within the
/// same window (resume's reset, possibly coalesced with a pause), a window moved without a fire,
/// or a window that appeared or vanished is a transition the manifest must persist.
fn is_fire_window_bookkeeping(
    previous: &MonitorSnapshotEntry,
    next: &MonitorSnapshotEntry,
) -> bool {
    let fired = next.fire_count.unwrap_or(0) > previous.fire_count.unwrap_or(0);
    match (&previous.fire_window, &next.fire_window) {
        (None, None) => true,
        (Some(before), Some(after)) if before.start_ms == after.start_ms => {
            after.count >= before.count
        }
        (Some(_), Some(_)) => fired,
        _ => false,
    }
}

/// Whether `next` differs from `previous` only by fire bookkeeping (fire count, last fire, fire
/// window growth or rollover): the registry's `onFire`, as opposed to its `onChange` transition. A
/// coalesced change still reads as a transition.
fn is_fire_only(previous: &[MonitorSnapshotEntry], next: &[MonitorSnapshotEntry]) -> bool {
    let unfired = |entry: &MonitorSnapshotEntry| MonitorSnapshotEntry {
        fire_count: None,
        last_fired_at_ms: None,
        fire_window: None,
        ..entry.clone()
    };
    previous.len() == next.len()
        && previous.iter().zip(next).all(|(previous, next)| {
            unfired(previous) == unfired(next) && is_fire_window_bookkeeping(previous, next)
        })
}

/// Drives `onMonitorState` from the registry's state channel. The first snapshot (upstream
/// `bundle.bind`) and every transition (`onChange`) are emitted with `transition = true`, then the
/// manifest mirrors mute state and burned fire windows and prunes monitors that ended, in order.
/// A fire (`onFire`) emits the state event only: no wake-source state, no manifest observe.
pub fn bind_monitor_events(
    mut state: tokio::sync::watch::Receiver<Vec<MonitorSnapshotEntry>>,
    sender: Arc<ExtensionApi>,
    lifecycle: Lifecycle,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut previous: Option<Vec<MonitorSnapshotEntry>> = None;
        loop {
            let snapshot = state.borrow_and_update().clone();
            let transition = previous
                .as_deref()
                .is_none_or(|previous| !is_fire_only(previous, &snapshot));
            emit_monitor_state(&sender, &snapshot, transition);
            if transition
                && let Some(lifecycle) = lifecycle.lock().await.as_mut()
                && let Err(error) = lifecycle
                    .writer
                    .observe_monitor_state(&snapshot, manifest_now_ms())
                    .await
            {
                maho_ext_api::diagnostic(&format!(
                    "terminal manifest monitor sync failed: {error}"
                ));
            }
            previous = Some(snapshot);
            if state.changed().await.is_err() {
                return;
            }
        }
    })
}

/// Upstream `bundleSinks.onMonitorEnded`: a watch that ended in this process (its command exited,
/// it was killed or expired) never runs again, so its restore baseline goes. Disposal is a shutdown
/// or reload: the watch is suspended, not ended, and a later restore still needs the dir.
pub fn remove_ended_monitor_state_dir(event: &MonitorEndedEvent, ctx: Option<&ExtensionContext>) {
    if event.reason == MonitorEndedReason::Disposed {
        return;
    }
    let (Some(dir), Some(monitor_id)) = (
        crate::monitor_state_dir::terminal_state_dir(ctx),
        event.monitor_id.as_deref(),
    ) else {
        return;
    };
    if let Err(error) = crate::monitor_state_dir::remove_monitor_state_dir_sync(&dir, monitor_id)
        && let Some(ctx) = ctx
    {
        ctx.ui.notify(
            &format!("Could not remove the state dir of {monitor_id}: {error}"),
            maho_ext_api::types::NotificationType::Warning,
        );
    }
}

pub fn monitor_ended_payload(event: &MonitorEndedEvent) -> Value {
    json!({"id":event.id,"description":event.description,"startedAtMs":event.started_at_ms,"endedAtMs":event.ended_at_ms,"reason":event.reason.as_str(),"exitCode":event.exit_code,"fireCount":event.fire_count})
}

/// Upstream `bundleSinks`: the sinks read the live state at call time, so notifier swaps need no
/// re-bind. Manifest writes run on the async runtime when one is current; without one (or without a
/// bound writer) nothing is recorded.
pub fn bundle_sinks(state: &TerminalExtensionState) -> TerminalEventSinks {
    let event_notifier = state.monitor_notifier.clone();
    let ended = state.clone();
    let monitor_state = state.clone();
    let background_state = state.clone();
    let background_exit = state.clone();
    TerminalEventSinks {
        on_monitor_event: Arc::new(move |event: MonitorEvent| {
            if let Some(notifier) = event_notifier.lock().expect("monitor notifier").as_ref()
                && let Err(error) = notifier.notify_event(event)
            {
                maho_ext_api::diagnostic(&format!("monitor delivery failed: {error}"));
            }
        }),
        on_monitor_ended: Arc::new(move |event: MonitorEndedEvent| {
            let payload = monitor_ended_payload(&event);
            let events = &ended.sender.events;
            events.emit(crate::shared::TERMINAL_MONITOR_ENDED_EVENT, &payload);
            events.emit(
                "senpi:extension-rpc-event",
                &json!({"name":crate::shared::TERMINAL_MONITOR_ENDED_EVENT,"data":payload}),
            );
            let ctx = ended
                .ctx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            remove_ended_monitor_state_dir(&event, ctx.as_ref());
        }),
        on_monitor_state: Arc::new(move |snapshot: &[MonitorSnapshotEntry], transition: bool| {
            emit_monitor_state(&monitor_state.sender, snapshot, transition);
            if !transition {
                return;
            }
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                return;
            };
            let (lifecycle, snapshot) = (monitor_state.lifecycle.clone(), snapshot.to_vec());
            handle.spawn(async move {
                if let Some(lifecycle) = lifecycle.lock().await.as_mut()
                    && let Err(error) = lifecycle
                        .writer
                        .observe_monitor_state(&snapshot, manifest_now_ms())
                        .await
                {
                    maho_ext_api::diagnostic(&format!(
                        "terminal manifest monitor sync failed: {error}"
                    ));
                }
            });
        }),
        on_background_state: Arc::new(move |snapshot: &[BackgroundSession]| {
            emit_background_state(snapshot, &background_state.sender);
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                return;
            };
            let (lifecycle, recorded, ensure, snapshot) = (
                background_state.lifecycle.clone(),
                background_state.recorded_background_ids.clone(),
                background_state.ensure_persistence.clone(),
                snapshot.to_vec(),
            );
            handle.spawn(async move {
                // Upstream: a live background session with no recorder bound yet binds
                // persistence first, then records.
                if !snapshot.is_empty() {
                    ensure_persistence(&ensure).await;
                }
                record_background_starts(&lifecycle, &recorded, snapshot).await;
            });
        }),
        on_background_exit: Arc::new(move |id: &str, view: &TerminalExitView| {
            let state = &background_exit;
            state
                .recorded_background_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(id);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let (lifecycle, id) = (state.lifecycle.clone(), id.to_owned());
                handle.spawn(async move {
                    if let Some(lifecycle) = lifecycle.lock().await.as_mut() {
                        lifecycle
                            .writer
                            .record_background_exit(&id, manifest_now_ms())
                            .await;
                    }
                });
            }
            send_completion_notice(id, view, &state.sender, &state.delivery, &state.notifier);
        }),
    }
}

/// Upstream `onBackgroundState` `record()`: each live background session is recorded once per
/// manifest writer; nothing is recorded while no writer is bound.
pub async fn record_background_starts(
    lifecycle: &Lifecycle,
    recorded: &RecordedBackgroundIds,
    snapshot: Vec<BackgroundSession>,
) {
    let mut lifecycle = lifecycle.lock().await;
    let Some(lifecycle) = lifecycle.as_mut() else {
        return;
    };
    for entry in snapshot {
        if !recorded
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(entry.id.clone())
        {
            continue;
        }
        lifecycle
            .writer
            .record_background_start(
                &entry.id,
                &entry.description,
                entry.started_at_ms,
                manifest_now_ms(),
            )
            .await;
    }
}

/// Upstream `buildToolContext`: the live configuration, the session cwd, the shell environment, and
/// the background hooks, which dispatch through the bundle sinks (`notifyBackgroundStart` /
/// `notifyBackgroundExit`).
pub fn build_tool_context(state: &TerminalExtensionState) -> TerminalToolContext {
    let mut context = TerminalToolContext::new(
        state.configuration.clone(),
        Arc::new(maho_core::utils::shell::get_shell_env),
    );
    context.session_cwd = state.session_cwd.clone();
    let sinks = bundle_sinks(state);
    let (backgrounds, on_state) = (state.backgrounds.clone(), sinks.on_background_state.clone());
    context.on_background_start = Some(Arc::new(
        move |id: &str, description: &str, started_at_ms: f64| {
            let snapshot = {
                let mut backgrounds = backgrounds
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                backgrounds.insert(
                    id.to_owned(),
                    BackgroundSession {
                        id: id.to_owned(),
                        description: description.to_owned(),
                        started_at_ms,
                    },
                );
                backgrounds.values().cloned().collect::<Vec<_>>()
            };
            on_state(snapshot.as_slice());
        },
    ));
    let backgrounds = state.backgrounds.clone();
    context.on_background_exit = Some(Arc::new(move |id: &str, view: TerminalExitView| {
        let snapshot = {
            let mut backgrounds = backgrounds
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            backgrounds.shift_remove(id);
            backgrounds.values().cloned().collect::<Vec<_>>()
        };
        (sinks.on_background_state)(snapshot.as_slice());
        (sinks.on_background_exit)(id, &view);
    }));
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::types::{
        EventBus, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, SourceInfo,
    };

    fn state(dir: &std::path::Path) -> TerminalExtensionState {
        TerminalExtensionState {
            sender: Arc::new(ExtensionApi::new(
                LoadedExtension::new("terminal", dir.to_owned(), SourceInfo::default()),
                ExtensionSessionProfile::default(),
                EventBus::default(),
                ExtensionRuntime::default(),
            )),
            ctx: Arc::default(),
            configuration: Arc::new(Mutex::new((
                crate::settings::TERMINAL_SETTINGS_DEFAULTS,
                None,
            ))),
            session_cwd: Arc::default(),
            backgrounds: Arc::default(),
            lifecycle: Arc::default(),
            monitor_notifier: Arc::default(),
            notifier: Arc::default(),
            delivery: Arc::default(),
            recorded_background_ids: Arc::default(),
            ensure_persistence: Arc::default(),
        }
    }

    /// `ensurePersistence`: concurrent callers share one binding, a later call does not re-run it,
    /// and without a lazy binding it is a no-op.
    #[tokio::test]
    async fn lazy_persistence_binds_once_for_concurrent_callers() {
        let slot: EnsurePersistence = Arc::default();
        ensure_persistence(&slot).await;
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (release, gate) = tokio::sync::watch::channel(false);
        let counted = runs.clone();
        *slot.lock().unwrap() = Some(LazyPersistence::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut gate = gate.clone();
            Box::pin(async move {
                let _ = gate.wait_for(|open| *open).await;
            })
        }));
        let (first, second) = (slot.clone(), slot.clone());
        let first = tokio::spawn(async move { ensure_persistence(&first).await });
        let second = tokio::spawn(async move { ensure_persistence(&second).await });
        release.send_replace(true);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            first.await.unwrap();
            second.await.unwrap();
        })
        .await
        .unwrap();
        ensure_persistence(&slot).await;
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;
    fn record(
        state: &TerminalExtensionState,
        channels: &[&'static str],
    ) -> (Seen, Vec<maho_ext_api::types::BusSubscription>) {
        let seen: Seen = Arc::default();
        let subscriptions = channels
            .iter()
            .copied()
            .map(|channel: &'static str| {
                let seen = seen.clone();
                state.sender.events.on(
                    channel,
                    Arc::new(move |data: &Value| {
                        seen.lock()
                            .unwrap()
                            .push((channel.to_owned(), data.clone()))
                    }),
                )
            })
            .collect();
        (seen, subscriptions)
    }

    /// 3864-named-wait-readers.test.ts: terminal monitors are named by their stable monitorId, so
    /// runtime id churn does not republish a different wait signature.
    #[test]
    fn wake_source_names_monitors_by_their_stable_monitor_id() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let (seen, _subscriptions) = record(&state, &[crate::shared::WAKE_SOURCE_STATE_EVENT]);
        let sinks = bundle_sinks(&state);
        let monitor = |id: &str| MonitorSnapshotEntry {
            id: id.to_owned(),
            monitor_id: Some("mon_build".to_owned()),
            description: "Build finished".to_owned(),
            started_at_ms: 1_761_264_000_000.0,
            ..Default::default()
        };
        (sinks.on_monitor_state)(&[monitor("runtime-1")], true);
        (sinks.on_monitor_state)(&[monitor("runtime-2")], true);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].1["source"], "terminal-monitors");
        assert_eq!(seen[0].1["monitors"][0]["id"], "mon_build");
        assert_eq!(seen[0].1, seen[1].1);
    }

    /// `onMonitorState(snapshot, false)` publishes the state event (bus and RPC) but no wake-source
    /// transition.
    #[test]
    fn a_non_transition_state_skips_the_wake_source_event() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let (seen, _subscriptions) = record(
            &state,
            &[
                crate::shared::TERMINAL_MONITOR_STATE_EVENT,
                crate::shared::WAKE_SOURCE_STATE_EVENT,
                "senpi:extension-rpc-event",
            ],
        );
        (bundle_sinks(&state).on_monitor_state)(&[], false);
        let channels = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(channel, _)| channel.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            channels,
            [
                crate::shared::TERMINAL_MONITOR_STATE_EVENT,
                "senpi:extension-rpc-event"
            ]
        );
    }

    /// `onMonitorEnded` reaches the bus and the RPC envelope; with no session context no state dir
    /// is touched.
    #[test]
    fn a_monitor_ending_is_emitted_on_the_bus_and_over_rpc() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let (seen, _subscriptions) = record(
            &state,
            &[
                crate::shared::TERMINAL_MONITOR_ENDED_EVENT,
                "senpi:extension-rpc-event",
            ],
        );
        (bundle_sinks(&state).on_monitor_ended)(MonitorEndedEvent {
            id: "bash_1".into(),
            monitor_id: Some("mon_ENDED".into()),
            description: "watch".into(),
            started_at_ms: 1.0,
            ended_at_ms: 2.0,
            reason: MonitorEndedReason::Exit,
            exit_code: Some(0),
            fire_count: 3,
        });
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].1["reason"], "exit");
        assert_eq!(seen[0].1["fireCount"], 3);
        assert_eq!(
            seen[1].1,
            json!({"name":crate::shared::TERMINAL_MONITOR_ENDED_EVENT,"data":seen[0].1})
        );
    }

    /// `buildToolContext.onBackgroundStart/onBackgroundExit` dispatch through the bundle sinks: the
    /// background set is published in start order and an exit drops the session from it.
    #[test]
    fn tool_context_background_hooks_publish_through_the_sinks() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let (seen, _subscriptions) = record(&state, &[crate::shared::WAKE_SOURCE_STATE_EVENT]);
        let context = build_tool_context(&state);
        let start = context.on_background_start.clone().unwrap();
        start("bash_1", "build", 1.0);
        start("bash_2", "serve", 2.0);
        (context.on_background_exit.clone().unwrap())(
            "bash_1",
            TerminalExitView::new(None, String::new()),
        );
        let ids = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, data)| {
                assert_eq!(data["source"], "terminal-background-sessions");
                data["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["id"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            [vec!["bash_1"], vec!["bash_1", "bash_2"], vec!["bash_2"]]
        );
        assert_eq!(
            state.backgrounds.lock().unwrap().keys().collect::<Vec<_>>(),
            ["bash_2"]
        );
    }

    /// The tool context reads the live configuration and session cwd at call time.
    #[test]
    fn tool_context_reads_live_configuration_and_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let context = build_tool_context(&state);
        assert_eq!(context.shell_path(), None);
        state.configuration.lock().unwrap().1 = Some("/bin/zsh".to_owned());
        state.configuration.lock().unwrap().0.default_cols = 91.0;
        *state.session_cwd.lock().unwrap() = Some(dir.path().to_owned());
        assert_eq!(context.shell_path().as_deref(), Some("/bin/zsh"));
        assert_eq!(context.settings().default_cols, 91.0);
        assert_eq!(context.cwd(), dir.path());
    }

    #[test]
    fn create_bundle_applies_the_session_capacity() {
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS;
        settings.max_sessions = 1.0;
        let bundle = create_bundle(&settings);
        let mut manager = bundle.manager.lock().unwrap();
        let reservation = manager.reserve().unwrap();
        assert!(reservation.is_some());
        assert!(manager.reserve().unwrap().is_none());
    }

    /// restore-session.ts `own` / extension.ts reload rebind: a background id recorded into
    /// writer A is recorded again into writer B once the writer is swapped.
    #[tokio::test]
    async fn a_swapped_writer_re_records_live_background_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let writer = |name: &str| {
            crate::terminal_state::TerminalStateLifecycle::new(&dir.path().join(name), "bg-swap")
        };
        let live = vec![BackgroundSession {
            id: "bash_1".to_owned(),
            description: "serve".to_owned(),
            started_at_ms: 1.0,
        }];
        async fn recorded_ids(
            lifecycle: &crate::terminal_state::TerminalStateLifecycle,
        ) -> Vec<String> {
            lifecycle
                .writer
                .read_restore_state()
                .await
                .unwrap()
                .map(|manifest| {
                    manifest
                        .background_sessions
                        .into_iter()
                        .map(|entry| entry.id)
                        .collect()
                })
                .unwrap_or_default()
        }
        assert!(
            replace_lifecycle(
                &mut *state.lifecycle.lock().await,
                &state.recorded_background_ids,
                Some(writer("a")),
            )
            .is_none()
        );
        record_background_starts(
            &state.lifecycle,
            &state.recorded_background_ids,
            live.clone(),
        )
        .await;
        let a = replace_lifecycle(
            &mut *state.lifecycle.lock().await,
            &state.recorded_background_ids,
            Some(writer("b")),
        )
        .unwrap();
        assert_eq!(recorded_ids(&a).await, ["bash_1"]);
        assert!(state.recorded_background_ids.lock().unwrap().is_empty());
        record_background_starts(&state.lifecycle, &state.recorded_background_ids, live).await;
        let b = replace_lifecycle(
            &mut *state.lifecycle.lock().await,
            &state.recorded_background_ids,
            None,
        )
        .unwrap();
        assert_eq!(recorded_ids(&b).await, ["bash_1"]);
        assert!(state.recorded_background_ids.lock().unwrap().is_empty());
    }

    /// session-bundle.ts: `onChange` is a transition and `onFire` is not, so a fire re-emits only
    /// the state event, never the wake-source state.
    #[tokio::test]
    async fn a_monitor_fire_is_not_a_transition() {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let (seen, mut received) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
        let _subscriptions = [
            crate::shared::TERMINAL_MONITOR_STATE_EVENT,
            crate::shared::WAKE_SOURCE_STATE_EVENT,
        ]
        .map(|channel: &'static str| {
            let seen = seen.clone();
            state.sender.events.on(
                channel,
                Arc::new(move |_: &Value| {
                    let _ = seen.send(channel);
                }),
            )
        });
        let watch = MonitorSnapshotEntry {
            id: "bash_1".to_owned(),
            monitor_id: Some("mon_FIRE".to_owned()),
            description: "watch".to_owned(),
            ..Default::default()
        };
        let fired = MonitorSnapshotEntry {
            fire_count: Some(1),
            last_fired_at_ms: Some(5.0),
            ..watch.clone()
        };
        let paused = MonitorSnapshotEntry {
            paused: true,
            ..fired.clone()
        };
        let (publish, subscribe) = tokio::sync::watch::channel(vec![watch]);
        let task = bind_monitor_events(subscribe, state.sender.clone(), state.lifecycle.clone());
        async fn next(
            received: &mut tokio::sync::mpsc::UnboundedReceiver<&'static str>,
        ) -> &'static str {
            tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
                .await
                .unwrap()
                .unwrap()
        }
        let state_event = crate::shared::TERMINAL_MONITOR_STATE_EVENT;
        let wake_event = crate::shared::WAKE_SOURCE_STATE_EVENT;
        assert_eq!(next(&mut received).await, state_event);
        assert_eq!(next(&mut received).await, wake_event);
        publish.send_replace(vec![fired]);
        assert_eq!(next(&mut received).await, state_event);
        publish.send_replace(vec![paused]);
        assert_eq!(next(&mut received).await, state_event);
        assert_eq!(next(&mut received).await, wake_event);
        drop(publish);
        task.await.unwrap();
        assert!(received.try_recv().is_err());
    }

    /// A pause and a resume coalesced into one snapshot leave `paused` unchanged but reset the
    /// fire window: that is a transition (the manifest must persist the reset), while a fire that
    /// rolls the window over is not.
    #[test]
    fn a_coalesced_pause_and_resume_is_a_transition() {
        use crate::monitor_registry::MonitorFireWindow;
        let entry = |fire_count: usize, start_ms: f64, count: usize| MonitorSnapshotEntry {
            id: "bash_1".to_owned(),
            monitor_id: Some("mon_PACED".to_owned()),
            fire_count: Some(fire_count),
            fire_window: Some(MonitorFireWindow { start_ms, count }),
            ..Default::default()
        };
        let burned = [entry(5, 100.0, 5)];
        assert!(is_fire_only(&burned, &[entry(6, 100.0, 6)]));
        assert!(is_fire_only(&burned, &[entry(6, 60_200.0, 1)]));
        assert!(!is_fire_only(&burned, &[entry(5, 100.0, 0)]));
        assert!(!is_fire_only(&burned, &[entry(7, 100.0, 2)]));
        assert!(!is_fire_only(&burned, &[entry(5, 60_200.0, 0)]));
        let unwindowed = MonitorSnapshotEntry {
            fire_window: None,
            ..entry(5, 0.0, 0)
        };
        assert!(!is_fire_only(&burned, &[unwindowed]));
    }

    /// The same coalesced pause+resume through `bind_monitor_events` emits the wake-source state.
    #[tokio::test]
    async fn a_coalesced_pause_and_resume_reaches_the_wake_source() {
        use crate::monitor_registry::MonitorFireWindow;
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path());
        let (seen, mut received) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
        let _subscriptions = [
            crate::shared::TERMINAL_MONITOR_STATE_EVENT,
            crate::shared::WAKE_SOURCE_STATE_EVENT,
        ]
        .map(|channel: &'static str| {
            let seen = seen.clone();
            state.sender.events.on(
                channel,
                Arc::new(move |_: &Value| {
                    let _ = seen.send(channel);
                }),
            )
        });
        let burned = MonitorSnapshotEntry {
            id: "bash_1".to_owned(),
            monitor_id: Some("mon_PACED".to_owned()),
            fire_count: Some(5),
            fire_window: Some(MonitorFireWindow {
                start_ms: 100.0,
                count: 5,
            }),
            ..Default::default()
        };
        let reset = MonitorSnapshotEntry {
            fire_window: Some(MonitorFireWindow {
                start_ms: 100.0,
                count: 0,
            }),
            ..burned.clone()
        };
        let (publish, subscribe) = tokio::sync::watch::channel(vec![burned]);
        let task = bind_monitor_events(subscribe, state.sender.clone(), state.lifecycle.clone());
        async fn next(
            received: &mut tokio::sync::mpsc::UnboundedReceiver<&'static str>,
        ) -> &'static str {
            tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
                .await
                .unwrap()
                .unwrap()
        }
        let state_event = crate::shared::TERMINAL_MONITOR_STATE_EVENT;
        let wake_event = crate::shared::WAKE_SOURCE_STATE_EVENT;
        assert_eq!(next(&mut received).await, state_event);
        assert_eq!(next(&mut received).await, wake_event);
        publish.send_replace(vec![reset]);
        assert_eq!(next(&mut received).await, state_event);
        assert_eq!(next(&mut received).await, wake_event);
        drop(publish);
        task.await.unwrap();
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_missing_context_has_no_session_key() {
        assert_eq!(session_key_of(None), None);
    }
}
