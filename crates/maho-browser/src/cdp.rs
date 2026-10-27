//! Chrome DevTools Protocol client over any message transport.
//!
//! Port of omowright `src/pipe.js` (`PipeCdpClient`: id sequence, pending-command map, per-command
//! timeout, event fan-out, fail-all on disconnect, dialog policy) and the WebSocket `CdpClient` of
//! `src/core.js` (same wire handling). The JS event emitter is replaced by a bounded event log with
//! sequence numbers that callers wait on, and the transport is a pair of closures so the pipe and
//! the WebSocket transports share one client.
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// omowright `DEFAULT_COMMAND_TIMEOUT_MS`.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Events kept for waiters; older ones are dropped first.
const EVENT_LOG_LIMIT: usize = 4096;

/// Sends one serialized CDP message.
pub type Writer = Box<dyn FnMut(&str) -> io::Result<()> + Send>;
/// Blocks for the next serialized CDP message; `Ok(None)` is end of stream.
pub type Reader = Box<dyn FnMut() -> io::Result<Option<String>> + Send>;

/// One CDP event as received.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: u64,
    pub method: String,
    pub session_id: Option<String>,
    pub params: Value,
}

#[derive(Default)]
struct State {
    pending: HashMap<u64, Option<Result<Value, String>>>,
    events: VecDeque<Event>,
    next_event_seq: u64,
    closed: Option<String>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    writer: Mutex<Writer>,
    next_id: AtomicU64,
    /// omowright dialog policy: `{ accept: true }` unless changed.
    accept_dialogs: Mutex<bool>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self, message: &str) -> io::Result<()> {
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        writer(message)
    }

    fn handle(&self, raw: &str) {
        let Ok(message) = serde_json::from_str::<Value>(raw) else {
            return;
        };
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            let mut state = self.lock();
            if let Some(slot) = state.pending.get_mut(&id) {
                *slot = Some(match message.get("error") {
                    Some(error) => Err(error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("CDP command failed")
                        .to_owned()),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                });
                self.changed.notify_all();
            }
            return;
        }
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return;
        };
        let session_id = message
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if method == "Page.javascriptDialogOpening"
            && let Some(session) = &session_id
        {
            let accept = *self
                .accept_dialogs
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let reply = json!({"id": id, "method": "Page.handleJavaScriptDialog", "params": {"accept": accept}, "sessionId": session});
            let _ = self.write(&reply.to_string());
        }
        let mut state = self.lock();
        let seq = state.next_event_seq;
        state.next_event_seq += 1;
        state.events.push_back(Event {
            seq,
            method: method.to_owned(),
            session_id,
            params: message.get("params").cloned().unwrap_or(Value::Null),
        });
        while state.events.len() > EVENT_LOG_LIMIT {
            state.events.pop_front();
        }
        self.changed.notify_all();
    }

    fn close(&self, reason: String) {
        let mut state = self.lock();
        if state.closed.is_none() {
            state.closed = Some(reason);
        }
        self.changed.notify_all();
    }
}

/// A connected CDP client. Cloning shares the connection.
#[derive(Clone)]
pub struct CdpClient {
    shared: Arc<Shared>,
    timeout: Duration,
}

impl CdpClient {
    /// Starts the reader thread over `reader` and sends through `writer`.
    pub fn new(writer: Writer, mut reader: Reader) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            accept_dialogs: Mutex::new(true),
        });
        let background = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("cdp-reader".into())
            .spawn(move || {
                loop {
                    match reader() {
                        Ok(Some(message)) => background.handle(&message),
                        Ok(None) => {
                            background.close("CDP connection closed by the browser".into());
                            return;
                        }
                        Err(error) => {
                            background.close(format!("CDP connection error: {error}"));
                            return;
                        }
                    }
                }
            })
            .map_or_else(
                |error| shared.close(format!("cannot start the CDP reader: {error}")),
                drop,
            );
        Self {
            shared,
            timeout: DEFAULT_COMMAND_TIMEOUT,
        }
    }

    /// Default per-command timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Whether JavaScript dialogs (alert/confirm/prompt/beforeunload) are accepted or dismissed.
    pub fn set_accept_dialogs(&self, accept: bool) {
        *self
            .shared
            .accept_dialogs
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = accept;
    }

    /// Why the connection ended, once it has.
    pub fn closed_reason(&self) -> Option<String> {
        self.shared.lock().closed.clone()
    }

    pub fn is_connected(&self) -> bool {
        self.closed_reason().is_none()
    }

    /// Sends `method` and waits for its result with the default timeout.
    pub fn send(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        self.send_with_timeout(method, params, session, self.timeout)
    }

    /// Sends `method` and waits at most `timeout` for its result.
    pub fn send_with_timeout(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
        timeout: Duration,
    ) -> Result<Value, String> {
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);
        let mut message = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        self.shared.lock().pending.insert(id, None);
        if let Err(error) = self.shared.write(&message.to_string()) {
            self.shared.lock().pending.remove(&id);
            return Err(format!("CDP write failed ({method}): {error}"));
        }
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        loop {
            if let Some(Some(_)) = state.pending.get(&id) {
                return match state.pending.remove(&id) {
                    Some(Some(Ok(value))) => Ok(value),
                    Some(Some(Err(error))) => Err(format!("{method}: {error}")),
                    _ => Err(format!("{method}: no result")),
                };
            }
            if let Some(reason) = &state.closed {
                let reason = reason.clone();
                state.pending.remove(&id);
                return Err(format!("{method}: {reason}"));
            }
            let now = Instant::now();
            if now >= deadline {
                state.pending.remove(&id);
                return Err(format!("CDP command timeout: {method}"));
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Sequence number the next event will get; pass it to [`Self::wait_event`] to see only
    /// events that arrive after this call.
    pub fn event_cursor(&self) -> u64 {
        self.shared.lock().next_event_seq
    }

    /// Events received at or after `cursor` that are still in the log.
    pub fn events_since(&self, cursor: u64) -> Vec<Event> {
        self.shared
            .lock()
            .events
            .iter()
            .filter(|event| event.seq >= cursor)
            .cloned()
            .collect()
    }

    /// Waits for the first event at or after `cursor` matching `accept`; `None` on timeout or
    /// disconnect.
    pub fn wait_event(
        &self,
        cursor: u64,
        timeout: Duration,
        accept: impl Fn(&Event) -> bool,
    ) -> Option<Event> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.lock();
        let mut scanned = cursor;
        loop {
            if let Some(event) = state
                .events
                .iter()
                .filter(|event| event.seq >= scanned)
                .find(|event| accept(event))
            {
                return Some(event.clone());
            }
            scanned = scanned.max(state.next_event_seq);
            if state.closed.is_some() {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            state = self
                .shared
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// An in-memory peer: messages the client writes come out of `sent`; lines pushed into
    /// `incoming` are read by the client.
    fn pair() -> (CdpClient, mpsc::Receiver<String>, mpsc::Sender<Option<String>>) {
        let (sent_tx, sent_rx) = mpsc::channel::<String>();
        let (in_tx, in_rx) = mpsc::channel::<Option<String>>();
        let writer: Writer = Box::new(move |message| {
            sent_tx
                .send(message.to_owned())
                .map_err(|_| io::Error::other("peer gone"))
        });
        let reader: Reader = Box::new(move || Ok(in_rx.recv().ok().flatten()));
        (CdpClient::new(writer, reader), sent_rx, in_tx)
    }

    fn sent_json(rx: &mpsc::Receiver<String>) -> Value {
        let raw = rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| String::from("{}"));
        serde_json::from_str(&raw).unwrap_or(Value::Null)
    }

    #[test]
    fn send_matches_results_by_id_and_carries_the_session() {
        let (client, sent, incoming) = pair();
        let worker = std::thread::spawn({
            let client = client.clone();
            move || client.send("Page.navigate", json!({"url": "about:blank"}), Some("S1"))
        });
        let request = sent_json(&sent);
        assert_eq!(request["method"], "Page.navigate");
        assert_eq!(request["sessionId"], "S1");
        let id = request["id"].as_u64().unwrap_or_default();
        let _ = incoming.send(Some(json!({"id": id + 100, "result": {}}).to_string()));
        let _ = incoming.send(Some(
            json!({"id": id, "result": {"frameId": "F"}}).to_string(),
        ));
        let result = worker.join().ok().and_then(Result::ok);
        assert_eq!(result, Some(json!({"frameId": "F"})));
    }

    #[test]
    fn protocol_errors_and_timeouts_are_reported() {
        let (client, sent, incoming) = pair();
        let worker = std::thread::spawn({
            let client = client.clone();
            move || client.send("Runtime.evaluate", json!({}), None)
        });
        let id = sent_json(&sent)["id"].as_u64().unwrap_or_default();
        let _ = incoming.send(Some(
            json!({"id": id, "error": {"code": -32000, "message": "boom"}}).to_string(),
        ));
        let error = worker.join().ok().and_then(Result::err);
        assert_eq!(error.as_deref(), Some("Runtime.evaluate: boom"));
        let timeout = client.send_with_timeout(
            "Browser.getVersion",
            json!({}),
            None,
            Duration::from_millis(50),
        );
        assert_eq!(
            timeout.err().as_deref(),
            Some("CDP command timeout: Browser.getVersion")
        );
    }

    #[test]
    fn events_are_logged_and_waited_on_from_a_cursor() {
        let (client, _sent, incoming) = pair();
        let _ = incoming.send(Some(
            json!({"method": "Page.lifecycleEvent", "params": {"name": "init"}, "sessionId": "S"})
                .to_string(),
        ));
        let first = client.wait_event(0, Duration::from_secs(5), |event| {
            event.method == "Page.lifecycleEvent"
        });
        assert_eq!(first.map(|event| event.params["name"].clone()), Some(json!("init")));
        let cursor = client.event_cursor();
        let _ = incoming.send(Some(
            json!({"method": "Page.lifecycleEvent", "params": {"name": "load"}, "sessionId": "S"})
                .to_string(),
        ));
        let load = client.wait_event(cursor, Duration::from_secs(5), |event| {
            event.params["name"] == "load"
        });
        assert_eq!(load.and_then(|event| event.session_id), Some("S".into()));
        assert_eq!(client.events_since(cursor).len(), 1);
    }

    #[test]
    fn dialogs_are_answered_by_policy() {
        let (client, sent, incoming) = pair();
        client.set_accept_dialogs(false);
        let _ = incoming.send(Some(
            json!({"method": "Page.javascriptDialogOpening", "params": {"type": "confirm", "message": "?"}, "sessionId": "S"})
                .to_string(),
        ));
        let reply = sent_json(&sent);
        assert_eq!(reply["method"], "Page.handleJavaScriptDialog");
        assert_eq!(reply["params"]["accept"], false);
        assert_eq!(reply["sessionId"], "S");
    }

    #[test]
    fn disconnect_fails_pending_and_later_commands() {
        let (client, sent, incoming) = pair();
        let worker = std::thread::spawn({
            let client = client.clone();
            move || client.send("Target.getTargets", json!({}), None)
        });
        let _ = sent_json(&sent);
        let _ = incoming.send(None);
        let error = worker.join().ok().and_then(Result::err).unwrap_or_default();
        assert!(error.contains("closed"), "{error}");
        assert!(!client.is_connected());
        assert!(client.send("Browser.getVersion", json!({}), None).is_err());
    }
}
