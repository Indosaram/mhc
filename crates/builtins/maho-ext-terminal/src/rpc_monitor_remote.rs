//! Port of `rpc-monitor-remote.ts` (senpi#6151):
//! The remote half of an rpc watch: one `RpcRemoteSource` subscription for a `<device>/<session>`
//! or `<device>/*` target. Records go through the watch's own render path with the device in the
//! socket's place, resyncs report status changes/closures/settled turns once, and `agent_settled`
//! is reported once per `turnId`.

use crate::rpc_monitor_coalesce::js_string;
use crate::rpc_monitor_render::{detached_label, session_key};
use crate::rpc_monitor_targets::RpcEvents;
use crate::rpc_monitor_watch::RpcHostIdentity;
use crate::rpc_remote_source::{
    RpcRemoteHandlers, RpcRemoteOpenOptions, RpcRemoteSettledTurn, RpcRemoteState,
    RpcRemoteSubscription, RpcRemoteTarget, SharedRpcRemoteSource,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

const REMOTE_INSTANCE: &str = "remote";
/// Turn ids remembered per session: enough to drop a resnapshot's repeats, bounded for a long watch.
const SETTLED_KEPT: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcRemoteCheckpoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub settled: BTreeMap<String, Vec<String>>,
}

pub trait RpcRemoteFeedHost: Send + Sync {
    fn seed(&self, device: &str, rows: &[Value]) -> Vec<String>;
    fn forget_socket(&self, device: &str);
    fn statuses(&self) -> BTreeMap<String, String>;
    fn host_identity(&self, device: &str) -> Option<RpcHostIdentity>;
    fn set_host_identity(&self, device: &str, identity: RpcHostIdentity);
    fn record(&self, socket: &str, record: &Value);
    fn line(&self, line: String);
    fn checkpoint(&self);
    fn ended(&self, summary: String);
}

#[derive(Clone)]
pub struct RpcRemoteFeedOptions {
    pub events: RpcEvents,
    pub exclude: Vec<String>,
    pub source: SharedRpcRemoteSource,
    pub saved: Option<RpcRemoteCheckpoint>,
}

enum DeferredStep {
    Record(Value),
    Resync(RpcRemoteState),
    Cursor(String),
    Lost(String),
    Ended(String),
}

struct FeedInner {
    target: RpcRemoteTarget,
    options: RpcRemoteFeedOptions,
    host: Arc<dyn RpcRemoteFeedHost>,
    state: Mutex<FeedState>,
}

struct FeedState {
    settled: HashMap<String, Vec<String>>,
    cursor: Option<String>,
    opened: Vec<RpcRemoteSettledTurn>,
    subscription: Option<RpcRemoteSubscription>,
    lost_reported: bool,
    stopped: bool,
    ready: bool,
    draining: bool,
    pending: VecDeque<DeferredStep>,
}

#[derive(Clone)]
pub struct RpcRemoteFeed {
    inner: Arc<FeedInner>,
}

fn turn_key(device: &str, turn: &RpcRemoteSettledTurn) -> String {
    format!("{device}#{}", turn.session_id)
}

fn settled_line(key: &str, turn_id: &str) -> String {
    format!(
        "rpc {} turn {turn_id} settled while detached",
        detached_label(key)
    )
}

impl FeedState {
    fn seen(&self, key: &str, turn_id: &str) -> bool {
        self.settled
            .get(key)
            .is_some_and(|turns| turns.iter().any(|existing| existing == turn_id))
    }

    fn remember(&mut self, key: &str, turn_id: &str) {
        let turns = self.settled.entry(key.to_owned()).or_default();
        if turns.iter().any(|existing| existing == turn_id) {
            return;
        }
        turns.push(turn_id.to_owned());
        if turns.len() > SETTLED_KEPT {
            turns.remove(0);
        }
    }
}

impl FeedInner {
    fn run_step(&self, step: DeferredStep) {
        let should_drain = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.stopped {
                return;
            }
            state.pending.push_back(step);
            if state.ready && !state.draining {
                state.draining = true;
                true
            } else {
                false
            }
        };
        if should_drain {
            self.drain();
        }
    }

    fn drain(&self) {
        struct ResetOnPanic<'a> {
            state: &'a Mutex<FeedState>,
            armed: bool,
        }
        impl Drop for ResetOnPanic<'_> {
            fn drop(&mut self) {
                if self.armed {
                    self.state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .draining = false;
                }
            }
        }
        let mut reset = ResetOnPanic {
            state: &self.state,
            armed: true,
        };
        loop {
            let step = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let next = if state.stopped {
                    None
                } else {
                    state.pending.pop_front()
                };
                let Some(step) = next else {
                    state.draining = false;
                    reset.armed = false;
                    return;
                };
                step
            };
            self.execute_step(step);
        }
    }

    fn execute_step(&self, step: DeferredStep) {
        match step {
            DeferredStep::Record(record) => self.handle_record(record),
            DeferredStep::Resync(remote_state) => self.handle_resync(remote_state),
            DeferredStep::Cursor(cursor) => {
                {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.stopped {
                        return;
                    }
                    state.cursor = Some(cursor);
                }
                self.host.checkpoint();
            }
            DeferredStep::Lost(reason) => {
                let emit = {
                    let mut state = self
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if state.stopped || state.lost_reported {
                        false
                    } else {
                        state.lost_reported = true;
                        true
                    }
                };
                if emit {
                    self.host.line(format!(
                        "rpc device {} unreachable ({reason}); re-watching",
                        self.target.device
                    ));
                }
            }
            DeferredStep::Ended(reason) => {
                if self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .stopped
                {
                    return;
                }
                self.host.line(format!(
                    "rpc {}/{} access ended ({reason})",
                    self.target.device, self.target.session
                ));
                self.host.ended(format!("access ended ({reason})"));
            }
        }
    }

    fn handle_record(&self, record: Value) {
        let device = &self.target.device;
        let kind = record.get("type").and_then(Value::as_str);
        let turn_id = record.get("turnId").and_then(Value::as_str);
        if kind != Some("agent_settled") || turn_id.is_none() {
            self.host.record(device, &record);
            return;
        }
        let turn_id = turn_id.unwrap_or_default();
        let key = session_key(device, &record);
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.stopped || state.seen(&key, turn_id) {
                return;
            }
            state.remember(&key, turn_id);
        }
        self.host.record(device, &record);
        self.host.checkpoint();
    }

    fn handle_resync(&self, remote_state: RpcRemoteState) {
        let device = &self.target.device;
        let mut lines = Vec::new();
        if let Some(generation) = &remote_state.generation {
            if let Some(previous) = self.host.host_identity(device)
                && previous.generation != *generation
            {
                lines.push(format!(
                    "rpc host {device} restarted (generation {} -> {generation})",
                    previous.generation
                ));
            }
            self.host.set_host_identity(
                device,
                RpcHostIdentity {
                    instance_id: REMOTE_INSTANCE.to_owned(),
                    generation: generation.clone(),
                },
            );
        }
        let prefix = format!("{device}#");
        let before: BTreeMap<String, String> = self
            .host
            .statuses()
            .into_iter()
            .filter(|(key, _)| key.starts_with(&prefix))
            .collect();
        let now: BTreeMap<String, String> = remote_state
            .rows
            .iter()
            .map(|row| (session_key(device, row), js_string(row.get("status"))))
            .collect();
        for (key, status) in &now {
            let was = before.get(key).map(String::as_str).unwrap_or("unknown");
            if was != status {
                lines.push(format!(
                    "rpc {} status {was} -> {status} (changed while detached)",
                    detached_label(key)
                ));
            }
        }
        for key in before.keys() {
            if !now.contains_key(key) {
                lines.push(format!(
                    "rpc {} closed (gone while detached)",
                    detached_label(key)
                ));
            }
        }
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for turn in remote_state.settled_turns.as_deref().unwrap_or(&[]) {
                let key = turn_key(device, turn);
                if state.seen(&key, &turn.turn_id) {
                    continue;
                }
                state.remember(&key, &turn.turn_id);
                lines.push(settled_line(&key, &turn.turn_id));
            }
            if let Some(cursor) = &remote_state.cursor {
                state.cursor = Some(cursor.clone());
            }
            state.lost_reported = false;
        }
        self.host.forget_socket(device);
        self.host.seed(device, &remote_state.rows);
        for line in lines {
            self.host.line(line);
        }
        self.host.checkpoint();
    }
}

impl RpcRemoteFeed {
    pub fn new(
        target: RpcRemoteTarget,
        options: RpcRemoteFeedOptions,
        host: Arc<dyn RpcRemoteFeedHost>,
    ) -> Self {
        let mut settled = HashMap::new();
        let cursor = options.saved.as_ref().and_then(|saved| {
            for (key, turns) in &saved.settled {
                settled.insert(key.clone(), turns.clone());
            }
            saved.cursor.clone()
        });
        Self {
            inner: Arc::new(FeedInner {
                target,
                options,
                host,
                state: Mutex::new(FeedState {
                    settled,
                    cursor,
                    opened: Vec::new(),
                    subscription: None,
                    lost_reported: false,
                    stopped: false,
                    ready: false,
                    draining: false,
                    pending: VecDeque::new(),
                }),
            }),
        }
    }

    pub async fn start(&self) -> Result<Vec<String>, String> {
        let resume_from = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cursor
            .clone();
        let weak_record = Arc::downgrade(&self.inner);
        let weak_resync = Arc::downgrade(&self.inner);
        let weak_cursor = Arc::downgrade(&self.inner);
        let weak_lost = Arc::downgrade(&self.inner);
        let weak_ended = Arc::downgrade(&self.inner);
        let subscription = self
            .inner
            .options
            .source
            .open(
                self.inner.target.clone(),
                RpcRemoteOpenOptions {
                    events: self.inner.options.events.clone(),
                    exclude: self.inner.options.exclude.clone(),
                    resume_from,
                },
                RpcRemoteHandlers {
                    record: Box::new(move |record| {
                        if let Some(feed) = weak_record.upgrade() {
                            feed.run_step(DeferredStep::Record(record));
                        }
                    }),
                    resynced: Box::new(move |state| {
                        if let Some(feed) = weak_resync.upgrade() {
                            feed.run_step(DeferredStep::Resync(state));
                        }
                    }),
                    cursor: Box::new(move |cursor| {
                        if let Some(feed) = weak_cursor.upgrade() {
                            feed.run_step(DeferredStep::Cursor(cursor));
                        }
                    }),
                    lost: Box::new(move |reason| {
                        if let Some(feed) = weak_lost.upgrade() {
                            feed.run_step(DeferredStep::Lost(reason));
                        }
                    }),
                    ended: Box::new(move |reason| {
                        if let Some(feed) = weak_ended.upgrade() {
                            feed.run_step(DeferredStep::Ended(reason));
                        }
                    }),
                },
            )
            .await?;

        let stopped = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped;
        if stopped {
            subscription.stop();
            return Ok(Vec::new());
        }

        let device = self.inner.target.device.clone();
        let generation = subscription.initial.generation.clone().unwrap_or_default();
        self.inner.host.set_host_identity(
            &device,
            RpcHostIdentity {
                instance_id: REMOTE_INSTANCE.to_owned(),
                generation,
            },
        );
        let initial_rows = subscription.initial.rows.clone();
        let settled_turns = subscription
            .initial
            .settled_turns
            .clone()
            .unwrap_or_default();
        let initial_cursor = subscription.initial.cursor.clone();
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.opened = settled_turns.clone();
            for turn in &settled_turns {
                let key = turn_key(&device, turn);
                state.remember(&key, &turn.turn_id);
            }
            if let Some(cursor) = initial_cursor {
                state.cursor = Some(cursor);
            }
            state.subscription = Some(subscription);
        }
        let rows = self.inner.host.seed(&device, &initial_rows);
        self.inner.host.checkpoint();
        let should_drain = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.ready = true;
            if !state.stopped && !state.draining && !state.pending.is_empty() {
                state.draining = true;
                true
            } else {
                false
            }
        };
        if should_drain {
            self.inner.drain();
        }
        Ok(rows)
    }

    pub fn stop(&self) {
        let subscription = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.stopped = true;
            state.pending.clear();
            state.subscription.take()
        };
        if let Some(subscription) = subscription {
            subscription.stop();
        }
    }

    pub fn checkpoint(&self) -> RpcRemoteCheckpoint {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let settled = state
            .settled
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        RpcRemoteCheckpoint {
            cursor: state.cursor.clone(),
            settled,
        }
    }

    pub fn diff_settled(&self, saved: Option<&RpcRemoteCheckpoint>) -> Vec<String> {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let device = &self.inner.target.device;
        state
            .opened
            .iter()
            .filter(|turn| {
                let key = turn_key(device, turn);
                !saved
                    .and_then(|saved| saved.settled.get(&key))
                    .is_some_and(|turns| turns.iter().any(|t| t == &turn.turn_id))
            })
            .map(|turn| settled_line(&turn_key(device, turn), &turn.turn_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_monitor_render::RpcMonitorRenderer;
    use crate::rpc_monitor_watch::RpcMonitorWatch;
    use crate::rpc_remote_source::{RpcRemoteOpenFuture, RpcRemoteSource};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    type RecordCallback = Arc<dyn Fn(&Value) + Send + Sync>;

    struct TestHost {
        renderer: Mutex<RpcMonitorRenderer>,
        hosts: Mutex<HashMap<String, RpcHostIdentity>>,
        lines: Mutex<Vec<String>>,
        records: Mutex<Vec<(String, Value)>>,
        ended: Mutex<Vec<String>>,
        checkpoints: Mutex<usize>,
        on_record: Mutex<Option<RecordCallback>>,
    }

    impl TestHost {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                renderer: Mutex::default(),
                hosts: Mutex::default(),
                lines: Mutex::default(),
                records: Mutex::default(),
                ended: Mutex::default(),
                checkpoints: Mutex::default(),
                on_record: Mutex::default(),
            })
        }
    }

    impl RpcRemoteFeedHost for TestHost {
        fn seed(&self, device: &str, rows: &[Value]) -> Vec<String> {
            self.renderer.lock().expect("renderer").seed(device, rows)
        }
        fn forget_socket(&self, device: &str) {
            self.renderer
                .lock()
                .expect("renderer")
                .forget_socket(device);
        }
        fn statuses(&self) -> BTreeMap<String, String> {
            self.renderer.lock().expect("renderer").statuses()
        }
        fn host_identity(&self, device: &str) -> Option<RpcHostIdentity> {
            self.hosts.lock().expect("hosts").get(device).cloned()
        }
        fn set_host_identity(&self, device: &str, identity: RpcHostIdentity) {
            self.hosts
                .lock()
                .expect("hosts")
                .insert(device.to_owned(), identity);
        }
        fn record(&self, socket: &str, record: &Value) {
            self.records
                .lock()
                .expect("records")
                .push((socket.to_owned(), record.clone()));
            let callback = self.on_record.lock().expect("on record").clone();
            if let Some(callback) = callback {
                callback(record);
            }
            if let Some(line) = self
                .renderer
                .lock()
                .expect("renderer")
                .render(socket, record)
            {
                self.lines.lock().expect("lines").push(line);
            }
        }
        fn line(&self, line: String) {
            self.lines.lock().expect("lines").push(line);
        }
        fn checkpoint(&self) {
            *self.checkpoints.lock().expect("checkpoints") += 1;
        }
        fn ended(&self, summary: String) {
            self.ended.lock().expect("ended").push(summary);
        }
    }

    struct StubSource {
        handlers: Mutex<Option<RpcRemoteHandlers>>,
        stopped: Arc<AtomicBool>,
    }

    impl RpcRemoteSource for StubSource {
        fn open(
            &self,
            _target: RpcRemoteTarget,
            _options: RpcRemoteOpenOptions,
            handlers: RpcRemoteHandlers,
        ) -> RpcRemoteOpenFuture {
            *self.handlers.lock().expect("handlers") = Some(handlers);
            let stopped = self.stopped.clone();
            Box::pin(async move {
                Ok(RpcRemoteSubscription::new(
                    RpcRemoteState {
                        rows: vec![json!({"sessionId":"abc1234567","status":"busy"})],
                        settled_turns: Some(vec![RpcRemoteSettledTurn {
                            session_id: "abc1234567".to_owned(),
                            turn_id: "t1".to_owned(),
                        }]),
                        generation: Some("g1".to_owned()),
                        cursor: Some("c1".to_owned()),
                    },
                    move || stopped.store(true, Ordering::SeqCst),
                ))
            })
        }
    }

    #[tokio::test]
    async fn events_arriving_while_pending_steps_drain_preserve_order() {
        let source = Arc::new(StubSource {
            handlers: Mutex::default(),
            stopped: Arc::new(AtomicBool::new(false)),
        });
        let host = TestHost::new();
        let feed = RpcRemoteFeed::new(
            RpcRemoteTarget {
                device: "laptop".to_owned(),
                session: "*".to_owned(),
            },
            RpcRemoteFeedOptions {
                events: RpcEvents::All,
                exclude: vec![],
                source,
                saved: None,
            },
            host.clone(),
        );
        let inner = Arc::downgrade(&feed.inner);
        *host.on_record.lock().expect("on record") = Some(Arc::new(move |record| {
            if record["id"] == "first" {
                inner
                    .upgrade()
                    .expect("feed alive")
                    .run_step(DeferredStep::Record(json!({"id":"third"})));
            }
        }));
        feed.inner
            .run_step(DeferredStep::Record(json!({"id":"first"})));
        feed.inner
            .run_step(DeferredStep::Record(json!({"id":"second"})));
        feed.start().await.expect("start feed");
        let records = host.records.lock().expect("records");
        let ids: Vec<_> = records
            .iter()
            .map(|(_, record)| record["id"].clone())
            .collect();
        assert_eq!(ids, [json!("first"), json!("second"), json!("third")]);
        drop(records);
        feed.stop();
    }

    #[tokio::test]
    async fn dedupes_settled_turns_reports_lost_once_and_resyncs() {
        let stopped = Arc::new(AtomicBool::new(false));
        let source = Arc::new(StubSource {
            handlers: Mutex::default(),
            stopped: stopped.clone(),
        });
        let host = TestHost::new();
        let feed = RpcRemoteFeed::new(
            RpcRemoteTarget {
                device: "laptop".to_owned(),
                session: "*".to_owned(),
            },
            RpcRemoteFeedOptions {
                events: RpcEvents::All,
                exclude: vec![],
                source: source.clone(),
                saved: None,
            },
            host.clone(),
        );
        let rows = feed.start().await.expect("remote start");
        assert_eq!(rows, vec!["abc12345 busy (0 attached)"]);
        assert_eq!(
            feed.diff_settled(None),
            vec!["rpc abc12345 turn t1 settled while detached"]
        );

        let handlers = source
            .handlers
            .lock()
            .expect("handlers")
            .take()
            .expect("handlers");
        // Duplicate settled turn t1 is ignored; t2 is delivered once.
        (handlers.record)(json!({"type":"agent_settled","sessionId":"abc1234567","turnId":"t1"}));
        (handlers.record)(json!({"type":"agent_settled","sessionId":"abc1234567","turnId":"t2"}));
        assert_eq!(host.records.lock().expect("records").len(), 1);

        // Lost reported once until resync clears it.
        (handlers.lost)("timeout".to_owned());
        (handlers.lost)("timeout".to_owned());
        (handlers.resynced)(RpcRemoteState {
            rows: vec![json!({"sessionId":"abc1234567","status":"idle"})],
            settled_turns: Some(vec![RpcRemoteSettledTurn {
                session_id: "abc1234567".to_owned(),
                turn_id: "t3".to_owned(),
            }]),
            generation: Some("g2".to_owned()),
            cursor: Some("c2".to_owned()),
        });
        (handlers.ended)("revoked".to_owned());

        let lines = host.lines.lock().expect("lines").clone();
        assert!(
            lines
                .iter()
                .any(|l| l == "rpc device laptop unreachable (timeout); re-watching")
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("rpc device laptop unreachable"))
                .count(),
            1
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "rpc host laptop restarted (generation g1 -> g2)")
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "rpc abc12345 status busy -> idle (changed while detached)")
        );
        assert!(
            lines
                .iter()
                .any(|l| l == "rpc abc12345 turn t3 settled while detached")
        );
        assert_eq!(feed.checkpoint().cursor.as_deref(), Some("c2"));
        assert_eq!(
            *host.ended.lock().expect("ended"),
            vec!["access ended (revoked)"]
        );
        feed.stop();
        assert!(stopped.load(Ordering::SeqCst));
        let _ = std::mem::size_of::<RpcMonitorWatch>();
    }
}
