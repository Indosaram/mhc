//! One rpc watch (senpi `rpc-monitor-watch.ts`): an observer per endpoint, joined again every 30 s
//! so hosts that start later are watched too, every delivered record turned into at most one line
//! (rendered, or folded into a coalescing window), and a checkpoint of what it last saw so a
//! restore can report what changed while nothing was watching.
use crate::rpc_monitor_coalesce::{RpcMonitorCoalescer, js_string};
use crate::rpc_monitor_observer::{RpcObserver, RpcObserverHandlers};
use crate::rpc_monitor_render::{RpcMonitorRenderer, session_key};
use crate::rpc_monitor_targets::{
    RpcEvents, RpcTarget, endpoint_sockets, matches_task, subscription_for, wants_record,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

pub const RPC_RESCAN_MS: u64 = 30_000;

/// One endpoint's host identity in a checkpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcHostIdentity {
    pub instance_id: String,
    pub generation: String,
}

/// What a restore compares against: each endpoint's host identity and each session's last status
/// (upstream `RpcMonitorCheckpoint`, persisted as the manifest's `rpcCheckpoint`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcMonitorCheckpoint {
    pub hosts: BTreeMap<String, RpcHostIdentity>,
    pub statuses: BTreeMap<String, String>,
}

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
/// Where to look for endpoints (upstream `RpcWatchOptions.endpoints`).
pub type EndpointLister = Arc<dyn Fn() -> BoxFuture<Vec<String>> + Send + Sync>;
/// How an endpoint is observed: `openRpcObserver` in production (the upstream module seam its
/// lifecycle test mocks).
pub type ObserverOpener = Arc<
    dyn Fn(String, Value, RpcObserverHandlers) -> BoxFuture<Result<RpcObserver, String>>
        + Send
        + Sync,
>;
pub type LineSink = Arc<dyn Fn(String) + Send + Sync>;
pub type CheckpointSink = Arc<dyn Fn(RpcMonitorCheckpoint) + Send + Sync>;

/// Upstream `RpcWatchOptions`.
pub struct RpcWatchOptions {
    pub agent_dir: std::path::PathBuf,
    pub target: RpcTarget,
    pub events: RpcEvents,
    pub exclude: Vec<String>,
    /// Defaults to every endpoint registered under `agent_dir`.
    pub endpoints: Option<EndpointLister>,
    /// Every line this watch reports.
    pub line: LineSink,
    /// The watch's view changed (a host joined, a status moved): persist it.
    pub checkpoint: Option<CheckpointSink>,
    /// Defaults to `open_rpc_observer`.
    pub opener: Option<ObserverOpener>,
}

type Close = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct WatchState {
    observers: HashMap<String, (u64, Close)>,
    /// Upstream `#hosts` Map: insertion order drives `diffSince`.
    hosts: indexmap::IndexMap<String, RpcHostIdentity>,
    renderer: RpcMonitorRenderer,
    task_sessions: HashSet<String>,
    rescan: Option<tokio::task::JoinHandle<()>>,
    stopped: bool,
    next_token: u64,
}

struct WatchInner {
    options: RpcWatchOptions,
    state: Mutex<WatchState>,
    coalescer: RpcMonitorCoalescer,
}

/// Upstream `RpcMonitorWatch`. Cloning shares the watch.
#[derive(Clone)]
pub struct RpcMonitorWatch {
    inner: Arc<WatchInner>,
}

fn default_opener() -> ObserverOpener {
    Arc::new(
        |socket: String,
         subscription: Value,
         handlers: RpcObserverHandlers|
         -> BoxFuture<Result<RpcObserver, String>> {
            Box::pin(async move {
                #[cfg(unix)]
                {
                    crate::rpc_monitor_observer::open_rpc_observer(
                        &socket,
                        &subscription,
                        handlers,
                        None,
                    )
                    .await
                }
                #[cfg(not(unix))]
                {
                    let _ = (socket, subscription, handlers);
                    Err("rpc monitor: unix sockets only".to_owned())
                }
            })
        },
    )
}

impl WatchInner {
    fn state(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn checkpoint(&self) -> RpcMonitorCheckpoint {
        let state = self.state();
        RpcMonitorCheckpoint {
            hosts: state.hosts.clone().into_iter().collect(),
            statuses: state.renderer.statuses(),
        }
    }

    fn emit_checkpoint(&self) {
        if let Some(sink) = &self.options.checkpoint {
            sink(self.checkpoint());
        }
    }

    async fn join(self: Arc<Self>) -> Vec<String> {
        let sockets = match &self.options.endpoints {
            Some(list) => list().await,
            None => endpoint_sockets(&self.options.agent_dir),
        };
        let fresh: Vec<String> = {
            let state = self.state();
            sockets
                .into_iter()
                .filter(|socket| !state.observers.contains_key(socket))
                .collect()
        };
        let tasks: Vec<_> = fresh
            .into_iter()
            .map(|socket| tokio::spawn(self.clone().observe(socket)))
            .collect();
        let mut rows = Vec::new();
        for task in tasks {
            rows.extend(task.await.unwrap_or_default());
        }
        rows
    }

    async fn observe(self: Arc<Self>, socket: String) -> Vec<String> {
        let token = {
            let mut state = self.state();
            if state.stopped {
                return vec![];
            }
            state.next_token += 1;
            state.next_token
        };
        let target = &self.options.target;
        let weak: Weak<Self> = Arc::downgrade(&self);
        let handlers = {
            let (record_watch, record_socket) = (weak.clone(), socket.clone());
            let (closed_watch, closed_socket) = (weak.clone(), socket.clone());
            let batch_watch = weak.clone();
            RpcObserverHandlers {
                record: Box::new(move |record| {
                    if let Some(watch) = record_watch.upgrade() {
                        watch.record(&record_socket, &record);
                    }
                }),
                closed: Box::new(move |_| {
                    if let Some(watch) = closed_watch.upgrade() {
                        watch.lost(&closed_socket, token);
                    }
                }),
                batch_end: Box::new(move || {
                    if let Some(watch) = batch_watch.upgrade() {
                        watch.state().renderer.end_batch();
                    }
                }),
            }
        };
        let opener = self.options.opener.clone().unwrap_or_else(default_opener);
        let subscription = subscription_for(target, &self.options.events);
        let Ok(observer) = opener(socket.clone(), subscription, handlers).await else {
            return vec![];
        };
        let (subscribed, close) = observer.into_parts();
        let close: Close = Arc::from(close);
        // A stop that landed while this observer was connecting has already closed every other one.
        {
            let mut state = self.state();
            if !state.stopped {
                state
                    .observers
                    .insert(socket.clone(), (token, close.clone()));
            }
        }
        if self.state().stopped {
            close();
            return vec![];
        }
        let answer = subscribed.await.ok().and_then(Result::ok);
        if self.state().stopped {
            close();
            return vec![];
        }
        let Some(data) = answer
            .as_ref()
            .and_then(|answer| answer.get("data"))
            .filter(|data| !data.is_null())
        else {
            return vec![];
        };
        let rows = {
            let mut state = self.state();
            state.hosts.insert(
                socket.clone(),
                RpcHostIdentity {
                    instance_id: js_string(data.get("hostInstanceId")),
                    generation: js_string(data.get("hostGeneration")),
                },
            );
            let matched: Vec<Value> = data
                .get("snapshot")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|row| matches_task(target, row))
                .cloned()
                .collect();
            for row in &matched {
                state.task_sessions.insert(session_key(&socket, row));
            }
            // An explicit id names one session: an endpoint whose snapshot did not match it is dropped.
            if !matches!(target, RpcTarget::All | RpcTarget::Children { .. }) && matched.is_empty()
            {
                state.observers.remove(&socket);
                drop(state);
                close();
                return vec![];
            }
            state.renderer.seed(&socket, &matched)
        };
        self.emit_checkpoint();
        rows
    }

    /// An endpoint's connection ended: say so once, and forget what it reported until it is joined again.
    fn lost(&self, socket: &str, token: u64) {
        let line = {
            let mut state = self.state();
            if state.observers.get(socket).map(|(own, _)| *own) != Some(token) {
                return;
            }
            state.observers.remove(socket);
            if state.stopped {
                return;
            }
            let forgotten = state.renderer.forget_socket(socket);
            let prefix = format!("{socket}#");
            state.task_sessions.retain(|key| !key.starts_with(&prefix));
            if state.hosts.shift_remove(socket).is_none() && forgotten == 0 {
                return;
            }
            format!(
                "rpc host {socket} gone ({forgotten} session{} no longer watched)",
                if forgotten == 1 { "" } else { "s" }
            )
        };
        (self.options.line)(line);
        self.emit_checkpoint();
    }

    fn record(self: &Arc<Self>, socket: &str, record: &Value) {
        let kind = js_string(record.get("type"));
        if !wants_record(&self.options.events, &self.options.exclude, &kind) {
            return;
        }
        let key = session_key(socket, record);
        {
            let mut state = self.state();
            if state.stopped {
                return;
            }
            if let RpcTarget::Task { .. } = self.options.target {
                if matches_task(&self.options.target, record) {
                    state.task_sessions.insert(key.clone());
                }
                if !state.task_sessions.contains(&key) {
                    return;
                }
            }
        }
        let label = {
            let (watch, socket, record) = (Arc::downgrade(self), socket.to_owned(), record.clone());
            move || {
                watch.upgrade().map_or_else(String::new, |watch| {
                    let mut state = watch.state();
                    let label = state.renderer.label(&socket, &record);
                    // The flush is its own batch (upstream's microtask clear after the timer).
                    state.renderer.end_batch();
                    label
                })
            }
        };
        if self.coalescer.accept(&key, label, record) {
            return;
        }
        let Some(line) = self.state().renderer.render(socket, record) else {
            return;
        };
        (self.options.line)(line);
        if kind == "session_status" {
            self.emit_checkpoint();
        }
    }
}

impl RpcMonitorWatch {
    pub fn new(options: RpcWatchOptions) -> Self {
        let line = options.line.clone();
        Self {
            inner: Arc::new(WatchInner {
                options,
                state: Mutex::default(),
                coalescer: RpcMonitorCoalescer::new(move |text| line(text)),
            }),
        }
    }

    /// Joins every endpoint now; resolves to the arm-time RESULT rows (one per matched session),
    /// then rejoins every `RPC_RESCAN_MS`.
    pub async fn start(&self) -> Vec<String> {
        let rows = self.inner.clone().join().await;
        let weak = Arc::downgrade(&self.inner);
        let rescan = tokio::spawn(async move {
            let period = std::time::Duration::from_millis(RPC_RESCAN_MS);
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            loop {
                interval.tick().await;
                let Some(watch) = weak.upgrade() else {
                    return;
                };
                watch.join().await;
            }
        });
        let mut state = self.inner.state();
        if state.stopped {
            rescan.abort();
        } else {
            state.rescan = Some(rescan);
        }
        rows
    }

    /// The current checkpoint: host identities and last statuses.
    pub fn checkpoint(&self) -> RpcMonitorCheckpoint {
        self.inner.checkpoint()
    }

    /// Lines for what changed between a saved checkpoint and what this watch sees now.
    pub fn diff_since(&self, saved: &RpcMonitorCheckpoint) -> Vec<String> {
        let state = self.inner.state();
        let mut lines = Vec::new();
        for (socket, host) in &state.hosts {
            // seq restarts with a generation, so only the generation says the host restarted.
            if let Some(before) = saved.hosts.get(socket)
                && before.generation != host.generation
            {
                lines.push(format!(
                    "rpc host {socket} restarted (generation {} -> {})",
                    before.generation, host.generation
                ));
            }
        }
        let now = state.renderer.statuses();
        let children = matches!(self.inner.options.target, RpcTarget::Children { .. });
        for (key, status) in &saved.statuses {
            let (socket, id) = match key.rfind('#') {
                Some(index) => (&key[..index], &key[index + 1..]),
                // `lastIndexOf` -1: the whole key labels, the socket drops its last character.
                None => (
                    key.char_indices()
                        .last()
                        .map_or("", |(index, _)| &key[..index]),
                    key.as_str(),
                ),
            };
            let label: String = id.chars().take(8).collect();
            // Only a host that answered can say a session is gone; otherwise it may be alive but
            // unreachable or no longer matched by the re-resolved target.
            let answered = state.hosts.contains_key(socket);
            match now.get(key) {
                None if answered && !children => {
                    lines.push(format!("rpc {label} closed (gone while detached)"))
                }
                None if answered => lines.push(format!(
                    "rpc {label} no longer listed (closed or no longer a child while detached)"
                )),
                None => lines.push(format!("rpc {label} unreachable (its host did not answer)")),
                Some(current) if current != status => lines.push(format!(
                    "rpc {label} status {status} -> {current} (changed while detached)"
                )),
                Some(_) => {}
            }
        }
        lines
    }

    pub fn stop(&self) {
        let observers: Vec<Close> = {
            let mut state = self.inner.state();
            state.stopped = true;
            if let Some(rescan) = state.rescan.take() {
                rescan.abort();
            }
            state
                .observers
                .drain()
                .map(|(_, (_, close))| close)
                .collect()
        };
        self.inner.coalescer.dispose();
        for close in observers {
            close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Opened {
        closed: Arc<AtomicBool>,
        handlers: RpcObserverHandlers,
    }
    /// The lifecycle test's mocked `openRpcObserver`: each open waits for `release`, then answers
    /// one busy session.
    #[derive(Clone, Default)]
    struct Mock {
        opened: Arc<Mutex<Vec<Opened>>>,
        release: Arc<tokio::sync::Notify>,
        entered: Arc<tokio::sync::Notify>,
    }
    impl Mock {
        fn opener(&self) -> ObserverOpener {
            let mock = self.clone();
            Arc::new(
                move |_socket: String,
                      _subscription: Value,
                      handlers: RpcObserverHandlers|
                      -> BoxFuture<Result<RpcObserver, String>> {
                    let mock = mock.clone();
                    Box::pin(async move {
                        mock.entered.notify_one();
                        mock.release.notified().await;
                        let closed = Arc::new(AtomicBool::new(false));
                        mock.opened.lock().unwrap().push(Opened {
                            closed: closed.clone(),
                            handlers,
                        });
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        let _ = tx.send(Ok(json!({"type":"response","success":true,"data":{
                        "hostInstanceId":"i-1","hostGeneration":"1",
                        "snapshot":[{"sessionId":"s1","status":"busy","attachments":1}]}})));
                        Ok(RpcObserver::new(rx, move || {
                            closed.store(true, Ordering::SeqCst)
                        }))
                    })
                },
            )
        }
    }

    fn watch(mock: &Mock, lines: &Arc<Mutex<Vec<String>>>, target: RpcTarget) -> RpcMonitorWatch {
        let sink = lines.clone();
        RpcMonitorWatch::new(RpcWatchOptions {
            agent_dir: "/tmp".into(),
            target,
            events: RpcEvents::Kinds(vec!["session_status".into()]),
            exclude: vec![],
            endpoints: Some(Arc::new(|| -> BoxFuture<Vec<String>> {
                Box::pin(async { vec!["/tmp/h.sock".to_owned()] })
            })),
            line: Arc::new(move |line| sink.lock().unwrap().push(line)),
            checkpoint: None,
            opener: Some(mock.opener()),
        })
    }

    #[tokio::test]
    async fn closes_an_observer_that_finished_connecting_after_the_watch_was_stopped() {
        let (mock, lines) = (Mock::default(), Arc::default());
        let subject = watch(&mock, &lines, RpcTarget::All);
        let started = tokio::spawn({
            let subject = subject.clone();
            async move { subject.start().await }
        });
        mock.entered.notified().await;
        subject.stop();
        mock.release.notify_one();
        assert!(started.await.unwrap().is_empty());
        let opened = mock.opened.lock().unwrap();
        assert_eq!(opened.len(), 1);
        assert!(opened[0].closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn reports_an_endpoint_whose_connection_ended_once_and_forgets_its_sessions() {
        let (mock, lines) = (Mock::default(), Arc::<Mutex<Vec<String>>>::default());
        let subject = watch(&mock, &lines, RpcTarget::All);
        mock.release.notify_one();
        assert_eq!(subject.start().await.len(), 1);
        {
            let opened = mock.opened.lock().unwrap();
            (opened[0].handlers.closed)("host gone".into());
            (opened[0].handlers.closed)("host gone".into());
        }
        let gone: Vec<String> = lines
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with("rpc host /tmp/h.sock gone"))
            .cloned()
            .collect();
        assert_eq!(
            gone,
            vec!["rpc host /tmp/h.sock gone (1 session no longer watched)"]
        );
        assert_eq!(subject.checkpoint(), RpcMonitorCheckpoint::default());
        subject.stop();
    }

    #[tokio::test]
    async fn says_unreachable_not_closed_for_a_saved_session_whose_host_did_not_answer() {
        let subject = watch(&Mock::default(), &Arc::default(), RpcTarget::All);
        let saved: RpcMonitorCheckpoint = serde_json::from_value(json!({
            "hosts":{"/tmp/other.sock":{"instanceId":"i","generation":"1"}},
            "statuses":{"/tmp/other.sock#abcdef123":"busy"}}))
        .unwrap();
        assert_eq!(
            subject.diff_since(&saved),
            vec!["rpc abcdef12 unreachable (its host did not answer)"]
        );
    }

    /// A status record is one line and re-checkpoints; a restore diff names the moved status, the
    /// session gone from an answering host and the host's new generation.
    #[tokio::test]
    async fn a_status_line_checkpoints_and_a_restore_diff_names_what_changed() {
        let mock = Mock::default();
        let lines = Arc::<Mutex<Vec<String>>>::default();
        let checkpoints = Arc::<Mutex<Vec<RpcMonitorCheckpoint>>>::default();
        let sink = lines.clone();
        let saved_sink = checkpoints.clone();
        let subject = RpcMonitorWatch::new(RpcWatchOptions {
            agent_dir: "/tmp".into(),
            target: RpcTarget::All,
            events: RpcEvents::Kinds(vec!["session_status".into()]),
            exclude: vec![],
            endpoints: Some(Arc::new(|| -> BoxFuture<Vec<String>> {
                Box::pin(async { vec!["/tmp/h.sock".to_owned()] })
            })),
            line: Arc::new(move |line| sink.lock().unwrap().push(line)),
            checkpoint: Some(Arc::new(move |checkpoint| {
                saved_sink.lock().unwrap().push(checkpoint)
            })),
            opener: Some(mock.opener()),
        });
        mock.release.notify_one();
        assert_eq!(subject.start().await, vec!["s1 busy (1 attached)"]);
        {
            let opened = mock.opened.lock().unwrap();
            (opened[0].handlers.record)(json!({"type":"agent_idle","sessionId":"s1"}));
            (opened[0].handlers.record)(
                json!({"type":"session_status","sessionId":"s1","status":"idle","seq":4}),
            );
            (opened[0].handlers.batch_end)();
        }
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["rpc s1 (s1) status busy -> idle seq=4"]
        );
        let latest = checkpoints.lock().unwrap().last().cloned().unwrap();
        assert_eq!(latest.statuses["/tmp/h.sock#s1"], "idle");
        assert_eq!(latest.hosts["/tmp/h.sock"].generation, "1");
        let saved: RpcMonitorCheckpoint = serde_json::from_value(json!({
            "hosts":{"/tmp/h.sock":{"instanceId":"i-0","generation":"0"}},
            "statuses":{"/tmp/h.sock#s1":"busy","/tmp/h.sock#s2":"idle"}}))
        .unwrap();
        assert_eq!(
            subject.diff_since(&saved),
            vec![
                "rpc host /tmp/h.sock restarted (generation 0 -> 1)",
                "rpc s1 status busy -> idle (changed while detached)",
                "rpc s2 closed (gone while detached)",
            ]
        );
        subject.stop();
    }
}
