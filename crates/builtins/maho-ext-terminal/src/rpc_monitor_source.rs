//! Rpc watches as a monitor source (senpi `rpc-monitor-source.ts`): each record owns one
//! `RpcMonitorWatch` and reports its lines through the registry's one sink, so the fire stats,
//! pause/rearm and the ended observer work as for command and file watches. A paused watch counts
//! the lines it drops; a line identical to the previous one is dropped (consecutive dedupe), on top
//! of the notifier's same-batch rule.
use crate::monitor_registry::{
    MonitorEndedEvent, MonitorEndedReason, MonitorEvent, MonitorSnapshotEntry, allocate_monitor_id,
};
use crate::rpc_monitor_targets::{RpcEvents, RpcTarget};
use crate::rpc_monitor_watch::{
    EndpointLister, ObserverOpener, RpcMonitorCheckpoint, RpcMonitorWatch, RpcWatchOptions,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

/// The summary an RPC watch settles with when its own deadline fires. An RPC watch observes other
/// sessions and kills nothing, so this must not claim a kill; the same constant feeds the
/// ended-reason classifier so the two can never drift.
pub const RPC_WATCHER_TIMED_OUT_SUMMARY: &str = "watcher timed_out";

pub type RpcCheckpointSink = Arc<dyn Fn(&str, RpcMonitorCheckpoint) + Send + Sync>;

/// Upstream `RegisterRpcMonitorOptions`.
#[derive(Clone)]
pub struct RegisterRpcMonitorOptions {
    pub description: String,
    pub target: RpcTarget,
    /// The target as the caller wrote it (what the manifest records).
    pub target_text: String,
    pub events: RpcEvents,
    pub exclude: Vec<String>,
    /// The filter's source text and its compiled form.
    pub filter: Option<(String, fancy_regex::Regex)>,
    pub persistent: bool,
    pub timeout_ms: Option<u64>,
    pub expires_at: Option<f64>,
    pub monitor_id: Option<String>,
    pub agent_dir: std::path::PathBuf,
    /// Endpoint discovery override (tests); defaults to the endpoints registered under `agent_dir`.
    pub endpoints: Option<EndpointLister>,
    /// Observer override (tests); defaults to `open_rpc_observer`.
    pub opener: Option<ObserverOpener>,
    /// A restore's saved view: the lines for what changed are injected once the watch has joined.
    pub restored_from: Option<RpcMonitorCheckpoint>,
    pub on_checkpoint: Option<RpcCheckpointSink>,
}

/// Upstream `RpcMonitorRecord`.
pub struct RpcMonitorRecord {
    pub snapshot: MonitorSnapshotEntry,
    pub watch: RpcMonitorWatch,
    pub target_text: String,
    pub muted_dropped: usize,
    last_line: Option<String>,
    filter: Option<fancy_regex::Regex>,
    deadline: Option<tokio::task::JoinHandle<()>>,
}

/// The registry's side of the source (upstream `MonitorSourceHost`).
#[derive(Clone)]
pub struct RpcSourceHost {
    pub emit: Arc<dyn Fn(MonitorEvent) + Send + Sync>,
    pub ended: Arc<dyn Fn(MonitorEndedEvent) + Send + Sync>,
    pub changed: Arc<dyn Fn() + Send + Sync>,
    pub now_ms: Arc<dyn Fn() -> f64 + Send + Sync>,
}

struct SourceInner {
    records: Mutex<indexmap::IndexMap<String, RpcMonitorRecord>>,
    host: Mutex<Option<RpcSourceHost>>,
    next_id: AtomicUsize,
}

/// Upstream `RpcMonitorSource`. Cloning shares the source.
#[derive(Clone)]
pub struct RpcMonitorSource {
    inner: Arc<SourceInner>,
}

fn ended_reason(summary: &str) -> MonitorEndedReason {
    match summary {
        RPC_WATCHER_TIMED_OUT_SUMMARY => MonitorEndedReason::Timeout,
        "watcher disposed" => MonitorEndedReason::Disposed,
        _ => MonitorEndedReason::Killed,
    }
}

impl SourceInner {
    fn records(&self) -> std::sync::MutexGuard<'_, indexmap::IndexMap<String, RpcMonitorRecord>> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn host(&self) -> Option<RpcSourceHost> {
        self.host
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn changed(&self) {
        if let Some(host) = self.host() {
            (host.changed)();
        }
    }

    fn line(&self, id: &str, line: String) {
        let Some(host) = self.host() else {
            return;
        };
        let event = {
            let mut records = self.records();
            let Some(record) = records.get_mut(id) else {
                return;
            };
            if record
                .filter
                .as_ref()
                .is_some_and(|filter| !filter.is_match(&line).unwrap_or(false))
            {
                return;
            }
            if record.last_line.as_deref() == Some(line.as_str()) {
                return;
            }
            record.last_line = Some(line.clone());
            if record.snapshot.paused {
                record.muted_dropped += 1;
                return;
            }
            // Upstream `#emitForRecord`: the fire stats move before the event leaves.
            record.snapshot.fire_count = Some(record.snapshot.fire_count.unwrap_or(0) + 1);
            record.snapshot.last_fired_at_ms = Some((host.now_ms)());
            MonitorEvent::Line {
                id: id.to_owned(),
                description: record.snapshot.description.clone(),
                line,
                human_input: false,
            }
        };
        (host.emit)(event);
        (host.changed)();
    }

    fn settle(&self, id: &str, summary: &str) -> bool {
        let Some(mut record) = self.records().shift_remove(id) else {
            return false;
        };
        record.watch.stop();
        if let Some(deadline) = record.deadline.take() {
            deadline.abort();
        }
        let Some(host) = self.host() else {
            return true;
        };
        (host.changed)();
        if summary != "watcher disposed" {
            (host.emit)(MonitorEvent::Summary {
                id: id.to_owned(),
                description: record.snapshot.description.clone(),
                summary: summary.to_owned(),
            });
        }
        (host.ended)(MonitorEndedEvent {
            id: id.to_owned(),
            monitor_id: record.snapshot.monitor_id.clone(),
            description: record.snapshot.description.clone(),
            started_at_ms: record.snapshot.started_at_ms,
            ended_at_ms: (host.now_ms)(),
            reason: ended_reason(summary),
            exit_code: None,
            fire_count: record.snapshot.fire_count.unwrap_or(0),
        });
        true
    }
}

impl RpcMonitorSource {
    pub fn new(host: RpcSourceHost) -> Self {
        Self {
            inner: Arc::new(SourceInner {
                records: Mutex::default(),
                host: Mutex::new(Some(host)),
                next_id: AtomicUsize::new(1),
            }),
        }
    }

    /// The live records' snapshots, in registration order.
    pub fn snapshots(&self) -> Vec<MonitorSnapshotEntry> {
        self.inner
            .records()
            .values()
            .map(|record| record.snapshot.clone())
            .collect()
    }

    pub fn keys(&self) -> Vec<String> {
        self.inner.records().keys().cloned().collect()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.inner.records().contains_key(id)
    }

    pub fn target_text(&self, id: &str) -> Option<String> {
        self.inner
            .records()
            .get(id)
            .map(|record| record.target_text.clone())
    }

    /// The live watch's checkpoint (what a manifest checkpoint records).
    pub fn checkpoint(&self, id: &str) -> Option<RpcMonitorCheckpoint> {
        let watch = self.inner.records().get(id)?.watch.clone();
        Some(watch.checkpoint())
    }

    pub fn muted_dropped(&self, id: &str) -> Option<usize> {
        self.inner
            .records()
            .get(id)
            .map(|record| record.muted_dropped)
    }

    /// Registry `pause` for an rpc record: true when it was live and running.
    pub fn pause(&self, id: &str) -> bool {
        let mut records = self.inner.records();
        let Some(record) = records.get_mut(id) else {
            return false;
        };
        !std::mem::replace(&mut record.snapshot.paused, true)
    }

    /// Registry `resume` for an rpc record: the lines dropped while paused, or `None` when it was
    /// not paused.
    pub fn resume(&self, id: &str) -> Option<usize> {
        let mut records = self.inner.records();
        let record = records
            .get_mut(id)
            .filter(|record| record.snapshot.paused)?;
        record.snapshot.paused = false;
        if let Some(window) = &mut record.snapshot.fire_window {
            window.count = 0;
        }
        Some(std::mem::take(&mut record.muted_dropped))
    }

    /// Arms the watch; resolves to its runtime id, stable id and the arm-time session rows.
    pub async fn register(
        &self,
        options: RegisterRpcMonitorOptions,
    ) -> std::io::Result<(String, String, Vec<String>)> {
        let (id, monitor_id, start) = self.arm(options)?;
        Ok((id, monitor_id, start.await))
    }

    /// The synchronous half of `register`: the record is live (snapshot, pause, settle) at once;
    /// the returned future joins the endpoints, injects a restore's detached-change lines and
    /// resolves to the arm-time rows. A restore applied under the registry lock spawns it.
    pub fn arm(
        &self,
        options: RegisterRpcMonitorOptions,
    ) -> std::io::Result<(
        String,
        String,
        impl std::future::Future<Output = Vec<String>> + Send + 'static,
    )> {
        let Some(host) = self.inner.host() else {
            return Err(std::io::Error::other(
                "Cannot create rpc monitor: monitor registry is disposed.",
            ));
        };
        let id = format!("rpc_{}", self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        let monitor_id = match options.monitor_id {
            Some(monitor_id) => monitor_id,
            None => allocate_monitor_id()?,
        };
        let weak: Weak<SourceInner> = Arc::downgrade(&self.inner);
        let line_id = id.clone();
        let line_source = weak.clone();
        let checkpoint = options.on_checkpoint.map(|sink| {
            let monitor_id = monitor_id.clone();
            Arc::new(move |checkpoint| sink(&monitor_id, checkpoint))
                as crate::rpc_monitor_watch::CheckpointSink
        });
        let watch = RpcMonitorWatch::new(RpcWatchOptions {
            agent_dir: options.agent_dir,
            target: options.target,
            events: options.events,
            exclude: options.exclude,
            endpoints: options.endpoints,
            line: Arc::new(move |line| {
                if let Some(source) = line_source.upgrade() {
                    source.line(&line_id, line);
                }
            }),
            checkpoint,
            opener: options.opener,
        });
        let now = (host.now_ms)();
        let timed = !options.persistent && options.timeout_ms.is_some();
        let deadline = options.timeout_ms.filter(|_| timed).map(|timeout_ms| {
            let (source, id) = (weak.clone(), id.clone());
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
                if let Some(source) = source.upgrade() {
                    source.settle(&id, RPC_WATCHER_TIMED_OUT_SUMMARY);
                }
            })
        });
        let (filter_text, filter) = options.filter.unzip();
        self.inner.records().insert(
            id.clone(),
            RpcMonitorRecord {
                snapshot: MonitorSnapshotEntry {
                    id: id.clone(),
                    monitor_id: Some(monitor_id.clone()),
                    description: options.description,
                    paused: false,
                    started_at_ms: now,
                    command: None,
                    filter: filter_text,
                    persistent: Some(options.persistent),
                    deadline_ms: options
                        .timeout_ms
                        .filter(|_| timed)
                        .map(|timeout_ms| now + timeout_ms as f64),
                    fire_count: Some(0),
                    last_fired_at_ms: None,
                    expires_at: options.expires_at,
                    fire_window: None,
                },
                watch: watch.clone(),
                target_text: options.target_text,
                muted_dropped: 0,
                last_line: None,
                filter,
                deadline,
            },
        );
        (host.changed)();
        let (source, start_id, restored_from) = (weak, id.clone(), options.restored_from);
        let start = async move {
            let rows = watch.start().await;
            if let (Some(saved), Some(source)) = (&restored_from, source.upgrade()) {
                for line in watch.diff_since(saved) {
                    source.line(&start_id, line);
                }
            }
            rows
        };
        Ok((id, monitor_id, start))
    }

    /// Ends one watch; false when the id is not a live rpc watch.
    pub fn settle(&self, id: &str, summary: &str) -> bool {
        self.inner.settle(id, summary)
    }

    pub fn settle_all(&self, summary: &str) -> usize {
        let ids = self.keys();
        for id in &ids {
            self.inner.settle(id, summary);
        }
        ids.len()
    }

    /// Registry `dispose`: every watch settles `watcher disposed` and nothing registers again.
    pub fn dispose(&self) {
        self.settle_all("watcher disposed");
        *self
            .inner
            .host
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_monitor_observer::{RpcObserver, RpcObserverHandlers};
    use crate::rpc_monitor_watch::BoxFuture;
    use serde_json::{Value, json};

    type Handlers = Arc<Mutex<Vec<RpcObserverHandlers>>>;
    struct Fixture {
        source: RpcMonitorSource,
        events: Arc<Mutex<Vec<MonitorEvent>>>,
        ended: Arc<Mutex<Vec<MonitorEndedEvent>>>,
        handlers: Handlers,
    }
    fn fixture() -> Fixture {
        let events = Arc::<Mutex<Vec<MonitorEvent>>>::default();
        let ended = Arc::<Mutex<Vec<MonitorEndedEvent>>>::default();
        let (emit, end) = (events.clone(), ended.clone());
        Fixture {
            source: RpcMonitorSource::new(RpcSourceHost {
                emit: Arc::new(move |event| emit.lock().unwrap().push(event)),
                ended: Arc::new(move |event| end.lock().unwrap().push(event)),
                changed: Arc::new(|| {}),
                now_ms: Arc::new(|| 1_000.0),
            }),
            events,
            ended,
            handlers: Arc::default(),
        }
    }
    fn options(fixture: &Fixture, filter: Option<&str>) -> RegisterRpcMonitorOptions {
        let handlers = fixture.handlers.clone();
        RegisterRpcMonitorOptions {
            description: "kids".into(),
            target: RpcTarget::All,
            target_text: "all".into(),
            events: RpcEvents::Kinds(vec!["session_status".into()]),
            exclude: vec![],
            filter: filter.map(|text| (text.to_owned(), fancy_regex::Regex::new(text).unwrap())),
            persistent: false,
            timeout_ms: Some(60_000),
            expires_at: None,
            monitor_id: Some("mon_RPC0000000000001".into()),
            agent_dir: "/tmp".into(),
            endpoints: Some(Arc::new(|| -> BoxFuture<Vec<String>> {
                Box::pin(async { vec!["/tmp/h.sock".to_owned()] })
            })),
            opener: Some(Arc::new(
                move |_socket: String,
                      _subscription: Value,
                      observer: RpcObserverHandlers|
                      -> BoxFuture<Result<RpcObserver, String>> {
                    handlers.lock().unwrap().push(observer);
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let _ = tx.send(Ok(
                        json!({"data":{"hostInstanceId":"i","hostGeneration":"2",
                        "snapshot":[{"sessionId":"s1","status":"busy"}]}}),
                    ));
                    Box::pin(async move { Ok(RpcObserver::new(rx, || {})) })
                },
            )),
            restored_from: None,
            on_checkpoint: None,
        }
    }
    fn status(fixture: &Fixture, to: &str) {
        let handlers = fixture.handlers.lock().unwrap();
        (handlers[0].record)(json!({"type":"session_status","sessionId":"s1","status":to,"seq":1}));
        (handlers[0].batch_end)();
    }
    fn lines(fixture: &Fixture) -> Vec<String> {
        fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                MonitorEvent::Line { line, .. } => Some(line.clone()),
                MonitorEvent::Summary { .. } => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn lines_are_filtered_deduped_counted_and_muted_while_paused() {
        let fixture = fixture();
        let (id, monitor_id, rows) = fixture
            .source
            .register(options(&fixture, Some("idle|busy")))
            .await
            .unwrap();
        assert_eq!(
            (id.as_str(), monitor_id.as_str()),
            ("rpc_1", "mon_RPC0000000000001")
        );
        assert_eq!(rows, vec!["s1 busy (0 attached)"]);
        status(&fixture, "idle");
        status(&fixture, "stopped");
        assert_eq!(
            lines(&fixture),
            vec!["rpc s1 (s1) status busy -> idle seq=1"]
        );
        let snapshot = &fixture.source.snapshots()[0];
        assert_eq!(snapshot.fire_count, Some(1));
        assert_eq!(snapshot.filter.as_deref(), Some("idle|busy"));
        assert_eq!(snapshot.deadline_ms, Some(61_000.0));
        assert!(fixture.source.pause(&id));
        status(&fixture, "busy");
        assert_eq!(lines(&fixture).len(), 1);
        assert_eq!(fixture.source.resume(&id), Some(1));
        assert_eq!(fixture.source.resume(&id), None);
        fixture.source.dispose();
    }

    #[tokio::test(start_paused = true)]
    async fn its_own_deadline_settles_it_timed_out_never_as_a_kill() {
        let fixture = fixture();
        let (id, ..) = fixture
            .source
            .register(options(&fixture, None))
            .await
            .unwrap();
        tokio::time::advance(std::time::Duration::from_millis(60_000)).await;
        tokio::task::yield_now().await;
        assert!(!fixture.source.contains(&id));
        assert_eq!(
            fixture.events.lock().unwrap().last(),
            Some(&MonitorEvent::Summary {
                id: id.clone(),
                description: "kids".into(),
                summary: "watcher timed_out".into(),
            })
        );
        let ended = fixture.ended.lock().unwrap();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].reason, MonitorEndedReason::Timeout);
        assert_eq!(ended[0].monitor_id.as_deref(), Some("mon_RPC0000000000001"));
    }

    #[tokio::test]
    async fn a_kill_says_killed_and_dispose_settles_silently() {
        let fixture = fixture();
        let (first, ..) = fixture
            .source
            .register(options(&fixture, None))
            .await
            .unwrap();
        let (second, ..) = fixture
            .source
            .register(options(&fixture, None))
            .await
            .unwrap();
        assert_eq!(second, "rpc_2");
        assert!(fixture.source.settle(&first, "watcher killed"));
        assert!(!fixture.source.settle(&first, "watcher killed"));
        fixture.source.dispose();
        let ended = fixture.ended.lock().unwrap();
        assert_eq!(
            ended
                .iter()
                .map(|event| event.reason.clone())
                .collect::<Vec<_>>(),
            vec![MonitorEndedReason::Killed, MonitorEndedReason::Disposed]
        );
        let summaries = fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|event| matches!(event, MonitorEvent::Summary { .. }))
            .count();
        assert_eq!(summaries, 1, "a disposed watch emits no summary");
        drop(ended);
        assert!(
            fixture
                .source
                .register(options(&fixture, None))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_restore_injects_what_changed_while_detached_and_checkpoints_by_monitor_id() {
        let fixture = fixture();
        let saved = Arc::<Mutex<Vec<(String, RpcMonitorCheckpoint)>>>::default();
        let sink = saved.clone();
        let mut restored = options(&fixture, None);
        restored.restored_from = Some(
            serde_json::from_value(
                json!({"hosts":{"/tmp/h.sock":{"instanceId":"i","generation":"1"}},
                "statuses":{"/tmp/h.sock#s1":"idle"}}),
            )
            .unwrap(),
        );
        restored.on_checkpoint = Some(Arc::new(move |monitor_id, checkpoint| {
            sink.lock()
                .unwrap()
                .push((monitor_id.to_owned(), checkpoint))
        }));
        fixture.source.register(restored).await.unwrap();
        assert_eq!(
            lines(&fixture),
            vec![
                "rpc host /tmp/h.sock restarted (generation 1 -> 2)",
                "rpc s1 status idle -> busy (changed while detached)",
            ]
        );
        let saved = saved.lock().unwrap();
        assert_eq!(saved[0].0, "mon_RPC0000000000001");
        assert_eq!(saved[0].1.statuses["/tmp/h.sock#s1"], "busy");
        fixture.source.dispose();
    }
}
