//! Buffered CDP network traffic for one page session.
//!
//! Port of omowright `src/network-snoop.js` (`createNetworkSnoop`): entries are tracked while in
//! flight and committed once they finish or fail, so filters may use response-only fields; bodies
//! are fetched with `Network.getResponseBody` unless oversized; `pop`/`peek`/`popJson`/`waitFor`/
//! `summary`/`dispose` keep their meaning.
//!
//! PORT-DIVERGENCE: the JS snoop subscribes listeners on the CDP emitter for the four request
//! lifecycle events. Here a [`Subscription`] filter for the same four events folds each one into
//! the snoop as it arrives (on the client's reader thread) and queues nothing, so memory stays
//! bounded by `maxEntries` whether or not the snoop is read. The instance `match` runs there too,
//! so it must not call the client. Response bodies cannot be fetched on the reader thread:
//! finished entries wait (counted against `maxEntries`) until the next read
//! ([`NetworkSnoop::poll`], called by every accessor) fetches their bodies and commits them, so a
//! body may have expired by then. `Network.enable` is sent on creation because the Rust page does
//! not enable the domain itself.
use crate::cdp::{CdpClient, Event, Subscription};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// omowright `DEFAULT_TIMEOUT_MS` for `waitFor`.
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Milliseconds since the Unix epoch (`Date.now()`).
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// One finished (or failed) request.
#[derive(Clone, Debug, PartialEq)]
pub struct NetworkEntry {
    pub request_id: String,
    pub url: String,
    pub method: String,
    pub resource_type: Option<String>,
    pub status: Option<i64>,
    pub status_text: Option<String>,
    pub mime_type: Option<String>,
    pub request_headers: Map<String, Value>,
    pub response_headers: Option<Map<String, Value>>,
    pub remote_ip_address: Option<String>,
    pub post_data: Option<String>,
    pub encoded_data_length: u64,
    pub body: Option<String>,
    pub base64_encoded: bool,
    pub body_skipped: bool,
    pub failed: bool,
    pub error_text: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl NetworkEntry {
    /// The entry as the JS snoop's plain object (camelCase keys).
    pub fn to_json(&self) -> Value {
        json!({
            "requestId": self.request_id,
            "url": self.url,
            "method": self.method,
            "resourceType": self.resource_type,
            "status": self.status,
            "statusText": self.status_text,
            "mimeType": self.mime_type,
            "requestHeaders": self.request_headers,
            "responseHeaders": self.response_headers,
            "remoteIPAddress": self.remote_ip_address,
            "postData": self.post_data,
            "encodedDataLength": self.encoded_data_length,
            "body": self.body,
            "base64Encoded": self.base64_encoded,
            "bodySkipped": self.body_skipped,
            "failed": self.failed,
            "errorText": self.error_text,
            "startedAt": self.started_at,
            "finishedAt": self.finished_at,
        })
    }

    /// The body as text (`decodeBody`): base64 bodies are decoded.
    pub fn decoded_body(&self) -> Option<String> {
        use base64::Engine as _;
        let body = self.body.as_ref()?;
        if !self.base64_encoded {
            return Some(body.clone());
        }
        base64::engine::general_purpose::STANDARD
            .decode(body)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// A text matcher (`string` = substring, or a predicate standing in for a `RegExp`).
pub enum TextMatch {
    Contains(String),
    /// Runs on the CDP reader thread under the client lock when used in [`SnoopOptions::matcher`]:
    /// it must not call the client or block.
    Test(Box<dyn Fn(&str) -> bool + Send + Sync>),
}

impl TextMatch {
    fn matches(&self, value: Option<&str>) -> bool {
        let Some(value) = value else {
            return false;
        };
        match self {
            Self::Contains(pattern) => value.contains(pattern.as_str()),
            Self::Test(test) => test(value),
        }
    }
}

/// The descriptor form of `match`: every given field must match.
#[derive(Default)]
pub struct MatchDescriptor {
    pub url: Option<TextMatch>,
    /// Case-insensitive equality.
    pub method: Option<String>,
    pub mime_type: Option<TextMatch>,
    /// Case-insensitive equality.
    pub resource_type: Option<String>,
}

/// `match`: nothing (everything), a descriptor, or a predicate.
#[derive(Default)]
pub enum EntryMatch {
    #[default]
    All,
    Descriptor(MatchDescriptor),
    /// Runs on the CDP reader thread under the client lock when used in [`SnoopOptions::matcher`]:
    /// it must not call the client or block.
    Predicate(Box<dyn Fn(&NetworkEntry) -> bool + Send + Sync>),
}

fn matches_token(value: Option<&str>, expected: Option<&String>) -> bool {
    match expected {
        None => true,
        Some(expected) => value.is_some_and(|value| value.eq_ignore_ascii_case(expected)),
    }
}

impl EntryMatch {
    /// A descriptor matching URLs that contain `pattern`.
    pub fn url(pattern: &str) -> Self {
        Self::Descriptor(MatchDescriptor {
            url: Some(TextMatch::Contains(pattern.to_owned())),
            ..MatchDescriptor::default()
        })
    }

    pub fn test(&self, entry: &NetworkEntry) -> bool {
        match self {
            Self::All => true,
            Self::Predicate(predicate) => predicate(entry),
            Self::Descriptor(descriptor) => {
                descriptor
                    .url
                    .as_ref()
                    .is_none_or(|url| url.matches(Some(entry.url.as_str())))
                    && matches_token(Some(entry.method.as_str()), descriptor.method.as_ref())
                    && descriptor
                        .mime_type
                        .as_ref()
                        .is_none_or(|mime| mime.matches(entry.mime_type.as_deref()))
                    && matches_token(
                        entry.resource_type.as_deref(),
                        descriptor.resource_type.as_ref(),
                    )
            }
        }
    }
}

/// `createNetworkSnoop` options.
pub struct SnoopOptions {
    /// Tested as each request finishes, on the CDP reader thread under the client lock: its
    /// predicates must not call the client or block.
    pub matcher: EntryMatch,
    pub bodies: bool,
    pub max_entries: usize,
    pub max_body_bytes: u64,
}

impl Default for SnoopOptions {
    fn default() -> Self {
        Self {
            matcher: EntryMatch::All,
            bodies: true,
            max_entries: 500,
            max_body_bytes: 5_000_000,
        }
    }
}

/// `application/json`, `...; charset`, or any `+json` type.
pub fn is_json_mime(mime: &str) -> bool {
    let lower = mime.to_ascii_lowercase();
    let essence = |text: &str| {
        let rest = text.trim_start();
        rest.is_empty() || rest.starts_with(';')
    };
    if let Some(rest) = lower.strip_prefix("application/json")
        && essence(rest)
    {
        return true;
    }
    let mut from = 0;
    while let Some(found) = lower[from..].find("+json") {
        let end = from + found + "+json".len();
        if essence(&lower[end..]) {
            return true;
        }
        from = end;
    }
    false
}

fn started_at_of(params: &Value) -> i64 {
    match params.get("wallTime").and_then(Value::as_f64) {
        Some(wall) if wall.is_finite() && wall > 0.0 => (wall * 1000.0).round() as i64,
        _ => now_ms(),
    }
}

fn string_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn object_at(value: &Value, pointer: &str) -> Option<Map<String, Value>> {
    value.pointer(pointer).and_then(Value::as_object).cloned()
}

/// The four events `poll` folds in (upstream `subscribe` calls).
const HANDLED_METHODS: [&str; 4] = [
    "Network.requestWillBeSent",
    "Network.responseReceived",
    "Network.loadingFinished",
    "Network.loadingFailed",
];

/// Snoop state folded on arrival by the subscription filter (on the client's reader thread).
struct Core {
    matcher: EntryMatch,
    max_entries: usize,
    inflight: HashMap<String, NetworkEntry>,
    /// Finished entries waiting for their body fetch, with the seq of their finishing event.
    pending: VecDeque<(u64, NetworkEntry)>,
    buffer: VecDeque<NetworkEntry>,
}

impl Core {
    /// Queues a finished entry, keeping `buffer` + `pending` within `max_entries`: the oldest
    /// entries go first, as they would leave upstream's buffer once committed.
    fn finish(&mut self, seq: u64, entry: NetworkEntry) {
        if !self.matcher.test(&entry) {
            return;
        }
        self.pending.push_back((seq, entry));
        while self.buffer.len() + self.pending.len() > self.max_entries {
            if self.buffer.pop_front().is_none() {
                self.pending.pop_front();
            }
        }
    }

    fn commit(&mut self, entry: NetworkEntry) {
        self.buffer.push_back(entry);
        while self.buffer.len() > self.max_entries {
            self.buffer.pop_front();
        }
    }

    /// Folds one of the [`HANDLED_METHODS`] events of the snooped session in.
    fn fold(&mut self, event: &Event) {
        let params = &event.params;
        let Some(request_id) = params.get("requestId").and_then(Value::as_str) else {
            return;
        };
        match event.method.as_str() {
            "Network.requestWillBeSent" => {
                let entry = NetworkEntry {
                    request_id: request_id.to_owned(),
                    url: string_at(params, "/request/url")
                        .or_else(|| string_at(params, "/documentURL"))
                        .unwrap_or_default(),
                    method: string_at(params, "/request/method").unwrap_or_else(|| "GET".into()),
                    resource_type: string_at(params, "/type"),
                    status: None,
                    status_text: None,
                    mime_type: None,
                    request_headers: object_at(params, "/request/headers").unwrap_or_default(),
                    response_headers: None,
                    remote_ip_address: None,
                    post_data: string_at(params, "/request/postData"),
                    encoded_data_length: 0,
                    body: None,
                    base64_encoded: false,
                    body_skipped: false,
                    failed: false,
                    error_text: None,
                    started_at: started_at_of(params),
                    finished_at: None,
                };
                self.inflight.insert(request_id.to_owned(), entry);
            }
            "Network.responseReceived" => {
                let Some(entry) = self.inflight.get_mut(request_id) else {
                    return;
                };
                entry.status = params.pointer("/response/status").and_then(Value::as_i64);
                entry.status_text =
                    Some(string_at(params, "/response/statusText").unwrap_or_default());
                entry.mime_type = string_at(params, "/response/mimeType");
                entry.response_headers =
                    Some(object_at(params, "/response/headers").unwrap_or_default());
                entry.remote_ip_address = string_at(params, "/response/remoteIPAddress");
                if let Some(kind) = string_at(params, "/type") {
                    entry.resource_type = Some(kind);
                }
            }
            "Network.loadingFinished" => {
                let Some(mut entry) = self.inflight.remove(request_id) else {
                    return;
                };
                entry.encoded_data_length = params
                    .get("encodedDataLength")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0) as u64;
                entry.finished_at = Some(now_ms());
                self.finish(event.seq, entry);
            }
            "Network.loadingFailed" => {
                let Some(mut entry) = self.inflight.remove(request_id) else {
                    return;
                };
                entry.failed = true;
                entry.error_text = Some(string_at(params, "/errorText").unwrap_or_default());
                if let Some(length) = params.get("encodedDataLength").and_then(Value::as_f64) {
                    entry.encoded_data_length = length as u64;
                }
                entry.finished_at = Some(now_ms());
                self.finish(event.seq, entry);
            }
            _ => {}
        }
    }
}

fn lock(core: &Mutex<Core>) -> MutexGuard<'_, Core> {
    core.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A network snoop over one CDP session.
pub struct NetworkSnoop {
    client: CdpClient,
    session_id: String,
    bodies: bool,
    max_body_bytes: u64,
    core: Arc<Mutex<Core>>,
    events: Option<Subscription>,
    disposed: bool,
}

impl NetworkSnoop {
    /// Starts buffering traffic of `session_id` from now on.
    pub fn new(client: CdpClient, session_id: &str, options: SnoopOptions) -> Self {
        let core = Arc::new(Mutex::new(Core {
            matcher: options.matcher,
            max_entries: options.max_entries,
            inflight: HashMap::new(),
            pending: VecDeque::new(),
            buffer: VecDeque::new(),
        }));
        let session = session_id.to_owned();
        let folded = Arc::clone(&core);
        // The filter folds each event in as it arrives and queues nothing.
        let events = client.subscribe(move |event| {
            if event.session_id.as_deref() == Some(session.as_str())
                && HANDLED_METHODS.contains(&event.method.as_str())
            {
                // A panicking user predicate drops this event instead of unwinding the reader
                // thread (upstream `enqueue` catches per task); `lock` recovers the poisoned core.
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| lock(&folded).fold(event)));
            }
            false
        });
        // Without the Network domain the page sends no request events.
        let _ = client.send("Network.enable", json!({}), Some(session_id));
        Self {
            client,
            session_id: session_id.to_owned(),
            bodies: options.bodies,
            max_body_bytes: options.max_body_bytes,
            core,
            events: Some(events),
            disposed: false,
        }
    }

    fn load_body(&self, entry: &mut NetworkEntry) {
        if entry.encoded_data_length > self.max_body_bytes {
            entry.body_skipped = true;
            return;
        }
        match self.client.send(
            "Network.getResponseBody",
            json!({"requestId": entry.request_id}),
            Some(&self.session_id),
        ) {
            Ok(result) => {
                entry.body = result
                    .get("body")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                entry.base64_encoded = result
                    .get("base64Encoded")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            }
            // Bodies expire with the network cache; keep the entry, drop the body.
            Err(_) => entry.body = None,
        }
    }

    /// Fetches the bodies of entries finished since the last read and commits them to the
    /// buffer; returns them with the seq of their finishing event.
    fn commit_pending(&mut self) -> Vec<(u64, NetworkEntry)> {
        let pending: Vec<_> = lock(&self.core).pending.drain(..).collect();
        let mut committed = Vec::with_capacity(pending.len());
        for (seq, mut entry) in pending {
            if self.bodies && !entry.failed {
                self.load_body(&mut entry);
            }
            lock(&self.core).commit(entry.clone());
            committed.push((seq, entry));
        }
        committed
    }

    /// Commits every entry finished since the last read; returns the entries it committed.
    pub fn poll(&mut self) -> Vec<NetworkEntry> {
        self.commit_pending()
            .into_iter()
            .map(|(_, entry)| entry)
            .collect()
    }

    /// Takes every buffered entry.
    pub fn pop(&mut self) -> Vec<NetworkEntry> {
        self.poll();
        lock(&self.core).buffer.drain(..).collect()
    }

    /// The buffered entries, left in place.
    pub fn peek(&mut self) -> Vec<NetworkEntry> {
        self.poll();
        lock(&self.core).buffer.iter().cloned().collect()
    }

    /// Takes every buffered entry and returns the parsed bodies of the JSON ones; unparsable or
    /// non-JSON entries are dropped silently.
    pub fn pop_json(&mut self) -> Vec<Value> {
        self.pop()
            .into_iter()
            .filter(|entry| is_json_mime(entry.mime_type.as_deref().unwrap_or_default()))
            .filter_map(|entry| entry.decoded_body())
            .filter_map(|text| serde_json::from_str(&text).ok())
            .collect()
    }

    /// The point in the event stream "now": entries whose final event arrives after it are
    /// candidates for [`Self::wait_from`].
    pub fn mark(&self) -> u64 {
        self.client.event_cursor()
    }

    /// Waits for the first entry committed after this call that matches `matcher`.
    pub fn wait_for(
        &mut self,
        matcher: &EntryMatch,
        timeout: Duration,
    ) -> Result<NetworkEntry, String> {
        let mark = self.mark();
        self.wait_from(mark, matcher, timeout)
    }

    /// Waits for the first entry matching `matcher` whose finishing event arrived at or after
    /// `mark` (see [`Self::mark`]). Earlier entries are buffered but never resolve the wait.
    pub fn wait_from(
        &mut self,
        mark: u64,
        matcher: &EntryMatch,
        timeout: Duration,
    ) -> Result<NetworkEntry, String> {
        if self.disposed {
            return Err("Network snoop is disposed".into());
        }
        let deadline = Instant::now() + timeout;
        loop {
            for (seq, entry) in self.commit_pending() {
                if seq >= mark && matcher.test(&entry) {
                    return Ok(entry);
                }
            }
            if !self.client.is_connected() {
                return Err("Network snoop disposed: the browser connection closed".into());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!(
                    "Timed out waiting for network response after {}ms",
                    timeout.as_millis()
                ));
            }
            if let Some(events) = &self.events {
                let core = Arc::clone(&self.core);
                events.wait_until(deadline - now, move || !lock(&core).pending.is_empty());
            }
        }
    }

    /// One line per entry, oldest first: `METHOD STATUS MIME SIZEB URL`, at most `max` lines.
    pub fn summary(&mut self, max: usize) -> String {
        self.poll();
        let core = lock(&self.core);
        let mut lines: Vec<String> = core
            .buffer
            .iter()
            .take(max)
            .map(|entry| {
                let status = match (entry.status, entry.failed) {
                    (Some(status), _) => status.to_string(),
                    (None, true) => "FAILED".into(),
                    (None, false) => "-".into(),
                };
                format!(
                    "{} {} {} {}B {}",
                    entry.method,
                    status,
                    entry.mime_type.as_deref().unwrap_or("-"),
                    entry.encoded_data_length,
                    entry.url
                )
            })
            .collect();
        let hidden = core.buffer.len().saturating_sub(max);
        if hidden > 0 {
            lines.push(format!("... +{hidden} more"));
        }
        lines.join("\n")
    }

    /// Stops recording; later reads see nothing new and waits fail.
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.events = None;
        let mut core = lock(&self.core);
        core.inflight.clear();
        core.pending.clear();
    }

    pub fn is_disposed(&self) -> bool {
        self.disposed
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake_cdp::FakeCdp;
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, PoisonError};

    type Bodies = Arc<Mutex<HashMap<String, Value>>>;

    fn fake() -> (FakeCdp, Bodies) {
        let bodies: Bodies = Arc::default();
        let lookup = Arc::clone(&bodies);
        let cdp = FakeCdp::new(Box::new(
            move |method: &str, params: &Value, _: Option<&str>| {
                if method == "Network.getResponseBody" {
                    let id = params["requestId"].as_str().unwrap_or_default();
                    return Ok(lookup
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| json!({"body": "", "base64Encoded": false})));
                }
                Ok(json!({}))
            },
        ));
        (cdp, bodies)
    }

    fn body(bodies: &Bodies, id: &str, body: &str, base64: bool) {
        bodies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.into(), json!({"body": body, "base64Encoded": base64}));
    }

    struct Req<'a> {
        url: &'a str,
        method: &'a str,
        post_data: Option<&'a str>,
        status: i64,
        mime: &'a str,
        length: u64,
        session: &'a str,
    }

    impl Default for Req<'_> {
        fn default() -> Self {
            Self {
                url: "https://x.test/api/items",
                method: "GET",
                post_data: None,
                status: 200,
                mime: "application/json",
                length: 12,
                session: "s1",
            }
        }
    }

    fn send_request(cdp: &FakeCdp, id: &str, req: &Req<'_>) {
        cdp.emit(
            "Network.requestWillBeSent",
            json!({"requestId": id, "request": {"url": req.url, "method": req.method, "headers": {"accept": "*/*"}, "postData": req.post_data}, "type": "XHR", "wallTime": 1_700_000_000, "timestamp": 1234.5}),
            Some(req.session),
        );
    }

    fn round_trip(cdp: &FakeCdp, id: &str, req: Req<'_>) {
        send_request(cdp, id, &req);
        let status_text = if req.status == 404 { "Not Found" } else { "OK" };
        cdp.emit(
            "Network.responseReceived",
            json!({"requestId": id, "type": "XHR", "response": {"status": req.status, "statusText": status_text, "mimeType": req.mime, "headers": {"content-type": req.mime}, "remoteIPAddress": "127.0.0.1"}}),
            Some(req.session),
        );
        cdp.emit(
            "Network.loadingFinished",
            json!({"requestId": id, "encodedDataLength": req.length}),
            Some(req.session),
        );
        cdp.flush();
    }

    fn snoop(cdp: &FakeCdp, options: SnoopOptions) -> NetworkSnoop {
        NetworkSnoop::new(cdp.client.clone(), "s1", options)
    }

    #[test]
    fn snoop_records_a_full_request_lifecycle_and_fetches_the_response_body() {
        let (cdp, bodies) = fake();
        body(&bodies, "r1", r#"{"ok":true}"#, false);
        let mut snoop = snoop(&cdp, SnoopOptions::default());
        round_trip(
            &cdp,
            "r1",
            Req {
                method: "POST",
                post_data: Some(r#"{"q":1}"#),
                length: 11,
                ..Req::default()
            },
        );
        let entries = snoop.peek();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.request_id, "r1");
        assert_eq!(entry.url, "https://x.test/api/items");
        assert_eq!(entry.method, "POST");
        assert_eq!(entry.resource_type.as_deref(), Some("XHR"));
        assert_eq!(entry.status, Some(200));
        assert_eq!(entry.status_text.as_deref(), Some("OK"));
        assert_eq!(entry.mime_type.as_deref(), Some("application/json"));
        assert_eq!(json!(entry.request_headers), json!({"accept": "*/*"}));
        assert_eq!(
            json!(entry.response_headers),
            json!({"content-type": "application/json"})
        );
        assert_eq!(entry.post_data.as_deref(), Some(r#"{"q":1}"#));
        assert_eq!(entry.encoded_data_length, 11);
        assert_eq!(entry.body.as_deref(), Some(r#"{"ok":true}"#));
        assert!(!entry.base64_encoded && !entry.body_skipped && !entry.failed);
        assert_eq!(entry.error_text, None);
        assert_eq!(entry.started_at, 1_700_000_000_000);
        assert!(entry.finished_at.unwrap_or_default() >= entry.started_at);
        let calls = cdp.calls_of("Network.getResponseBody");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].params, json!({"requestId": "r1"}));
        assert_eq!(calls[0].session.as_deref(), Some("s1"));
        assert_eq!(snoop.peek().len(), 1, "peek does not consume");
        assert_eq!(cdp.calls_of("Network.enable").len(), 1);
    }

    #[test]
    fn snoop_ignores_other_cdp_sessions_and_pop_consumes_once() {
        let (cdp, _) = fake();
        let mut snoop = snoop(&cdp, SnoopOptions::default());
        round_trip(
            &cdp,
            "other",
            Req {
                url: "https://other.test/api/items",
                session: "other",
                ..Req::default()
            },
        );
        round_trip(&cdp, "r1", Req::default());
        let first = snoop.pop();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].request_id, "r1");
        assert!(snoop.pop().is_empty());
        assert!(snoop.peek().is_empty());
    }

    #[test]
    fn traffic_older_than_the_client_event_log_is_still_recorded() {
        let (cdp, _) = fake();
        let mut snoop = snoop(&cdp, SnoopOptions::default());
        round_trip(&cdp, "early", Req::default());
        // More unrelated events than the client log keeps, all before the snoop is read.
        for index in 0..5000 {
            cdp.emit(
                "Page.lifecycleEvent",
                json!({"name": "noise", "index": index}),
                Some("s1"),
            );
        }
        round_trip(&cdp, "late", Req::default());
        let ids: Vec<String> = snoop
            .pop()
            .into_iter()
            .map(|entry| entry.request_id)
            .collect();
        assert_eq!(ids, ["early", "late"]);
    }

    #[test]
    fn an_unread_snoop_stays_within_max_entries_and_queues_no_raw_events() {
        let (cdp, _) = fake();
        let mut snoop = snoop(&cdp, SnoopOptions::default());
        for index in 0..600 {
            let id = format!("r{index}");
            round_trip(&cdp, &id, Req::default());
            for method in [
                "Network.dataReceived",
                "Network.requestWillBeSentExtraInfo",
                "Network.responseReceivedExtraInfo",
                "Network.webSocketFrameReceived",
            ] {
                cdp.emit(method, json!({"requestId": id}), Some("s1"));
            }
        }
        cdp.flush();
        let events = snoop
            .events
            .as_ref()
            .map_or(usize::MAX, Subscription::queued);
        assert_eq!(events, 0, "events are folded on arrival, not queued");
        {
            let core = lock(&snoop.core);
            assert_eq!(core.buffer.len() + core.pending.len(), 500);
            assert!(core.inflight.is_empty());
        }
        assert!(
            cdp.calls_of("Network.getResponseBody").is_empty(),
            "nothing read yet"
        );
        let ids: Vec<String> = snoop.pop().into_iter().map(|e| e.request_id).collect();
        let expected: Vec<String> = (100..600).map(|index| format!("r{index}")).collect();
        assert_eq!(ids, expected, "the oldest 100 entries were evicted");
        assert_eq!(cdp.calls_of("Network.getResponseBody").len(), 500);
    }

    #[test]
    fn pop_json_parses_json_bodies_and_silently_drops_unparsable_or_non_json_entries() {
        let (cdp, bodies) = fake();
        body(&bodies, "r1", r#"[{"id":1}]"#, false);
        body(&bodies, "r2", "not json at all", false);
        body(&bodies, "r3", "<html></html>", false);
        body(&bodies, "r4", "eyJhIjoyfQ==", true);
        let mut snoop = snoop(&cdp, SnoopOptions::default());
        round_trip(&cdp, "r1", Req::default());
        round_trip(
            &cdp,
            "r2",
            Req {
                url: "https://x.test/api/broken",
                ..Req::default()
            },
        );
        round_trip(
            &cdp,
            "r3",
            Req {
                url: "https://x.test/page",
                mime: "text/html",
                ..Req::default()
            },
        );
        round_trip(
            &cdp,
            "r4",
            Req {
                url: "https://x.test/api/ld",
                mime: "application/ld+json",
                ..Req::default()
            },
        );
        assert_eq!(snoop.pop_json(), vec![json!([{"id": 1}]), json!({"a": 2})]);
        assert!(snoop.pop().is_empty(), "popJson drains the buffer");
    }

    #[test]
    fn an_instance_match_filters_which_entries_are_buffered() {
        let (cdp, _) = fake();
        let mut snoop = snoop(
            &cdp,
            SnoopOptions {
                matcher: EntryMatch::Descriptor(MatchDescriptor {
                    url: Some(TextMatch::Contains("/api/".into())),
                    method: Some("get".into()),
                    mime_type: Some(TextMatch::Test(Box::new(|mime: &str| {
                        mime.contains("json")
                    }))),
                    resource_type: Some("xhr".into()),
                }),
                bodies: false,
                ..SnoopOptions::default()
            },
        );
        round_trip(&cdp, "r1", Req::default());
        round_trip(
            &cdp,
            "r2",
            Req {
                url: "https://x.test/page",
                ..Req::default()
            },
        );
        round_trip(
            &cdp,
            "r3",
            Req {
                method: "POST",
                ..Req::default()
            },
        );
        round_trip(
            &cdp,
            "r4",
            Req {
                mime: "text/html",
                ..Req::default()
            },
        );
        let ids: Vec<String> = snoop.pop().into_iter().map(|e| e.request_id).collect();
        assert_eq!(ids, ["r1"]);
    }

    #[test]
    fn a_panicking_predicate_drops_that_event_and_later_requests_are_still_recorded() {
        let (cdp, _) = fake();
        let mut snoop = snoop(
            &cdp,
            SnoopOptions {
                matcher: EntryMatch::Predicate(Box::new(|entry: &NetworkEntry| {
                    assert_ne!(entry.request_id, "boom", "user predicate panics");
                    true
                })),
                bodies: false,
                ..SnoopOptions::default()
            },
        );
        round_trip(&cdp, "r1", Req::default());
        round_trip(&cdp, "boom", Req::default());
        round_trip(&cdp, "r2", Req::default());
        let ids: Vec<String> = snoop.pop().into_iter().map(|e| e.request_id).collect();
        assert_eq!(ids, ["r1", "r2"]);
    }

    #[test]
    fn wait_for_resolves_on_a_later_matching_entry_and_rejects_on_timeout() {
        let (cdp, bodies) = fake();
        body(&bodies, "r2", r#"{"ok":true}"#, false);
        let mut snoop = snoop(&cdp, SnoopOptions::default());
        round_trip(
            &cdp,
            "r0",
            Req {
                url: "https://x.test/api/click",
                ..Req::default()
            },
        );
        let mark = snoop.mark();
        round_trip(&cdp, "r1", Req::default());
        round_trip(
            &cdp,
            "r2",
            Req {
                url: "https://x.test/api/click",
                ..Req::default()
            },
        );
        let entry = snoop
            .wait_from(mark, &EntryMatch::url("/api/click"), Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            entry.request_id, "r2",
            "only entries finishing after the call resolve the waiter"
        );
        assert_eq!(entry.body.as_deref(), Some(r#"{"ok":true}"#));
        let missed = snoop.wait_for(
            &EntryMatch::Predicate(Box::new(|candidate: &NetworkEntry| {
                candidate.url.contains("never-happens")
            })),
            Duration::from_millis(20),
        );
        assert!(
            missed
                .err()
                .is_some_and(|error| error.starts_with("Timed out waiting for network response"))
        );
    }

    #[test]
    fn summary_prints_one_line_per_entry_oldest_first_and_truncates() {
        let (cdp, _) = fake();
        let mut snoop = snoop(
            &cdp,
            SnoopOptions {
                bodies: false,
                ..SnoopOptions::default()
            },
        );
        round_trip(
            &cdp,
            "r1",
            Req {
                url: "https://x.test/api/1",
                length: 120,
                ..Req::default()
            },
        );
        round_trip(
            &cdp,
            "r2",
            Req {
                url: "https://x.test/api/2",
                method: "POST",
                status: 404,
                length: 33,
                ..Req::default()
            },
        );
        round_trip(
            &cdp,
            "r3",
            Req {
                url: "https://x.test/api/3",
                mime: "text/html",
                length: 7,
                ..Req::default()
            },
        );
        assert_eq!(
            snoop.summary(50),
            [
                "GET 200 application/json 120B https://x.test/api/1",
                "POST 404 application/json 33B https://x.test/api/2",
                "GET 200 text/html 7B https://x.test/api/3",
            ]
            .join("\n")
        );
        assert_eq!(
            snoop.summary(2),
            [
                "GET 200 application/json 120B https://x.test/api/1",
                "POST 404 application/json 33B https://x.test/api/2",
                "... +1 more",
            ]
            .join("\n")
        );
    }

    #[test]
    fn max_entries_evicts_the_oldest_entries_and_bodies_false_skips_body_fetches() {
        let (cdp, _) = fake();
        let mut snoop = snoop(
            &cdp,
            SnoopOptions {
                max_entries: 2,
                bodies: false,
                ..SnoopOptions::default()
            },
        );
        for id in ["r1", "r2", "r3"] {
            round_trip(&cdp, id, Req::default());
        }
        let ids: Vec<String> = snoop.peek().into_iter().map(|e| e.request_id).collect();
        assert_eq!(ids, ["r2", "r3"]);
        assert!(cdp.calls_of("Network.getResponseBody").is_empty());
    }

    #[test]
    fn oversized_bodies_are_skipped_and_failed_requests_are_recorded() {
        let (cdp, bodies) = fake();
        body(&bodies, "r1", &"x".repeat(50), false);
        let mut snoop = snoop(
            &cdp,
            SnoopOptions {
                max_body_bytes: 10,
                ..SnoopOptions::default()
            },
        );
        round_trip(
            &cdp,
            "r1",
            Req {
                url: "https://x.test/api/big",
                length: 999,
                ..Req::default()
            },
        );
        send_request(
            &cdp,
            "r2",
            &Req {
                url: "https://x.test/api/dead",
                ..Req::default()
            },
        );
        cdp.emit(
            "Network.loadingFailed",
            json!({"requestId": "r2", "errorText": "net::ERR_ABORTED", "encodedDataLength": 0}),
            Some("s1"),
        );
        cdp.flush();
        let popped = snoop.pop();
        assert_eq!(popped.len(), 2);
        assert!(popped[0].body_skipped);
        assert_eq!(popped[0].body, None);
        assert!(cdp.calls_of("Network.getResponseBody").is_empty());
        assert!(popped[1].failed);
        assert_eq!(popped[1].error_text.as_deref(), Some("net::ERR_ABORTED"));
        assert_eq!(popped[1].status, None);
    }

    #[test]
    fn dispose_rejects_waiters_and_stops_recording() {
        let (cdp, _) = fake();
        let mut snoop = snoop(&cdp, SnoopOptions::default());
        snoop.dispose();
        round_trip(&cdp, "r1", Req::default());
        assert!(snoop.peek().is_empty());
        let waited = snoop.wait_for(&EntryMatch::url("/never"), Duration::from_secs(5));
        assert!(waited.err().is_some_and(|error| error.contains("disposed")));
        snoop.dispose();
    }

    #[test]
    fn json_mime_detection_follows_the_upstream_pattern() {
        for good in [
            "application/json",
            "application/json; charset=utf-8",
            "application/ld+json",
            "APPLICATION/JSON",
        ] {
            assert!(is_json_mime(good), "{good}");
        }
        for bad in ["text/html", "application/jsonx", "text/plain+jsonish", ""] {
            assert!(!is_json_mime(bad), "{bad}");
        }
    }
}
