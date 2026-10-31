//! Browser-level tab and download events.
//!
//! Port of omowright `src/events.js` (`createEvents`, `BrowserEventBridge`): target discovery
//! (`tabOpened` with `initial`, `popupOpened` when the opener is a known tab, `tabClosed` with the
//! crash reason and the last known info), download tracking (`Browser.setDownloadBehavior` with
//! events enabled, `downloadStarted` mapped to the owning tab through frame and session ids,
//! `downloadProgress`, one `downloadFinished` per guid, in-flight downloads failed with
//! `browserDisconnected` when the transport drops) and `waitForEvent`.
//!
//! PORT-DIVERGENCE: the JS bridge pushes events to listeners. Here a [`Subscription`] queues the
//! target, download and frame CDP events without loss, [`BrowserEvents::poll`] folds them in and
//! returns the bridge events in order; listeners become the caller's loop. The Rust client has no reconnect, so the
//! `reconnected` re-configuration has no trigger here; a disconnect is detected from
//! [`CdpClient::closed_reason`].
use crate::cdp::{CdpClient, Event, Subscription};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// omowright `BROWSER_EVENT_NAMES`.
pub const BROWSER_EVENT_NAMES: [&str; 6] = [
    "tabOpened",
    "tabClosed",
    "popupOpened",
    "downloadStarted",
    "downloadProgress",
    "downloadFinished",
];
/// omowright `DEFAULT_TIMEOUT` for `waitForEvent`.
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const TERMINAL_LIMIT: usize = 4096;

/// One bridge event: `name` is one of [`BROWSER_EVENT_NAMES`], `payload` the JS payload object.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserEvent {
    pub name: &'static str,
    pub payload: Value,
}

/// `downloadBehavior`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DownloadBehavior {
    #[default]
    Default,
    Deny,
    /// Save under the absolute directory.
    Allow(String),
    /// Save under the directory with the guid as file name.
    AllowAndName(String),
}

fn path_is_absolute(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

impl DownloadBehavior {
    /// `#validate`: `allow` needs an absolute path.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Allow(path) if !path_is_absolute(path) => {
                Err("downloadPath must be an absolute path for allow".into())
            }
            _ => Ok(()),
        }
    }

    fn params(&self) -> Value {
        match self {
            Self::Default => json!({"behavior": "default", "eventsEnabled": true}),
            Self::Deny => json!({"behavior": "deny", "eventsEnabled": true}),
            Self::Allow(path) => {
                json!({"behavior": "allow", "eventsEnabled": true, "downloadPath": path})
            }
            Self::AllowAndName(path) => {
                json!({"behavior": "allowAndName", "eventsEnabled": true, "downloadPath": path})
            }
        }
    }
}

fn page_target(info: &Value) -> Option<&Value> {
    (info.get("type").and_then(Value::as_str) == Some("page")
        && info
            .get("targetId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()))
    .then_some(info)
}

fn normalize_target(info: &Value) -> Value {
    let text = |key: &str, default: &str| {
        info.get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_owned()
    };
    let nullable = |key: &str| info.get(key).filter(|value| !value.is_null()).cloned();
    json!({
        "targetId": text("targetId", ""),
        "type": "page",
        "url": text("url", "about:blank"),
        "title": text("title", ""),
        "openerId": nullable("openerId"),
        "openerFrameId": nullable("openerFrameId"),
        "browserContextId": nullable("browserContextId"),
        "attached": info.get("attached").and_then(Value::as_bool).unwrap_or(false),
        "subtype": nullable("subtype"),
    })
}

#[derive(Clone, Debug)]
struct Download {
    guid: String,
    url: Value,
    suggested_filename: String,
    frame_id: Value,
    target_id: Value,
    total_bytes: f64,
    received_bytes: f64,
    file_path: Value,
}

/// `createEvents(connection, options)`.
pub struct BrowserEvents {
    client: CdpClient,
    behavior: DownloadBehavior,
    events: Option<Subscription>,
    targets: BTreeMap<String, Value>,
    target_order: Vec<String>,
    crashes: HashMap<String, Value>,
    downloads: BTreeMap<String, Download>,
    terminal_downloads: HashMap<String, Download>,
    terminal: VecDeque<String>,
    frame_to_target: HashMap<String, String>,
    session_to_target: HashMap<String, String>,
    pending: VecDeque<BrowserEvent>,
    disconnected: bool,
    disposed: bool,
}

impl BrowserEvents {
    /// Starts target discovery, lists the open tabs (reported as `initial`) and configures
    /// downloads.
    pub fn new(client: CdpClient, behavior: DownloadBehavior) -> Result<Self, String> {
        behavior.validate()?;
        let subscription = client.subscribe(|event| {
            event.method.starts_with("Target.")
                || event.method.starts_with("Browser.download")
                || matches!(
                    event.method.as_str(),
                    "Page.frameAttached" | "Page.frameNavigated"
                )
        });
        let mut events = Self {
            client,
            behavior,
            events: Some(subscription),
            targets: BTreeMap::new(),
            target_order: Vec::new(),
            crashes: HashMap::new(),
            downloads: BTreeMap::new(),
            terminal_downloads: HashMap::new(),
            terminal: VecDeque::new(),
            frame_to_target: HashMap::new(),
            session_to_target: HashMap::new(),
            pending: VecDeque::new(),
            disconnected: false,
            disposed: false,
        };
        events
            .client
            .send("Target.setDiscoverTargets", json!({"discover": true}), None)?;
        let listed = events.client.send("Target.getTargets", json!({}), None)?;
        events.configure_downloads()?;
        let mut targets: Vec<Value> = listed
            .get("targetInfos")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(page_target)
            .cloned()
            .collect();
        // Openers first, so a popup listed at startup is classified against its opener.
        targets.sort_by_key(|info| info.get("openerId").is_some_and(|id| !id.is_null()));
        for info in &targets {
            events.created(info, true);
        }
        Ok(events)
    }

    fn configure_downloads(&self) -> Result<(), String> {
        self.client
            .send("Browser.setDownloadBehavior", self.behavior.params(), None)
            .map(drop)
    }

    fn dispatch(&mut self, name: &'static str, payload: Value) {
        self.pending.push_back(BrowserEvent { name, payload });
    }

    fn map_frame(&mut self, frame_id: &str, session_id: &str) {
        let target = self.session_to_target.get(session_id).cloned().or_else(|| {
            self.targets
                .values()
                .find(|info| info["attached"] == true && info["targetId"] == session_id)
                .and_then(|info| info["targetId"].as_str().map(str::to_owned))
        });
        if let Some(target) = target {
            self.frame_to_target.insert(frame_id.to_owned(), target);
        }
    }

    fn created(&mut self, info: &Value, initial: bool) {
        let Some(target) = page_target(info.get("targetInfo").unwrap_or(info)) else {
            return;
        };
        let normalized = normalize_target(target);
        let id = normalized["targetId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if self.targets.contains_key(&id) {
            return;
        }
        self.targets.insert(id.clone(), normalized.clone());
        self.target_order.push(id.clone());
        let opener = normalized["openerId"].as_str().map(str::to_owned);
        let popup = opener
            .as_ref()
            .is_some_and(|opener| self.targets.contains_key(opener));
        let mut opened = normalized.clone();
        opened["popup"] = json!(popup);
        opened["initial"] = json!(initial);
        self.dispatch("tabOpened", opened);
        if popup {
            self.dispatch(
                "popupOpened",
                json!({
                    "targetId": id, "openerTargetId": opener, "openerFrameId": normalized["openerFrameId"],
                    "url": normalized["url"], "title": normalized["title"], "initial": initial,
                }),
            );
        }
    }

    fn destroyed(&mut self, id: &str) {
        let Some(last) = self.targets.remove(id) else {
            return;
        };
        self.target_order.retain(|known| known != id);
        let crash = self.crashes.remove(id);
        self.frame_to_target.retain(|_, target| target != id);
        self.dispatch(
            "tabClosed",
            json!({
                "targetId": id,
                "reason": if crash.is_some() { "crashed" } else { "destroyed" },
                "lastKnown": last,
                "crash": crash,
            }),
        );
    }

    fn remember_terminal(&mut self, guid: &str) {
        self.terminal.retain(|known| known != guid);
        self.terminal.push_back(guid.to_owned());
        if self.terminal.len() > TERMINAL_LIMIT
            && let Some(oldest) = self.terminal.pop_front()
        {
            self.terminal_downloads.remove(&oldest);
        }
    }

    fn download_begin(&mut self, info: &Value) {
        let Some(guid) = info.get("guid").and_then(Value::as_str) else {
            return;
        };
        if self.terminal.iter().any(|known| known == guid) {
            return;
        }
        let frame_id = info.get("frameId").cloned().unwrap_or(Value::Null);
        let target_id = frame_id
            .as_str()
            .and_then(|frame| self.frame_to_target.get(frame))
            .map_or(Value::Null, |target| json!(target));
        let record = Download {
            guid: guid.to_owned(),
            url: info.get("url").cloned().unwrap_or(Value::Null),
            suggested_filename: info
                .get("suggestedFilename")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            frame_id,
            target_id,
            total_bytes: 0.0,
            received_bytes: 0.0,
            file_path: Value::Null,
        };
        self.dispatch(
            "downloadStarted",
            json!({
                "guid": record.guid, "url": record.url, "suggestedFilename": record.suggested_filename,
                "frameId": record.frame_id, "targetId": record.target_id,
            }),
        );
        self.downloads.insert(guid.to_owned(), record);
    }

    fn download_progress(&mut self, info: &Value) {
        let Some(guid) = info.get("guid").and_then(Value::as_str) else {
            return;
        };
        let already_terminal = self.terminal.iter().any(|known| known == guid);
        let record = match self.downloads.get_mut(guid) {
            Some(record) => record,
            None => match self.terminal_downloads.get_mut(guid) {
                Some(record) => record,
                None => return,
            },
        };
        let number = |key: &str| info.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        record.total_bytes = number("totalBytes");
        record.received_bytes = number("receivedBytes");
        record.file_path = info.get("filePath").cloned().unwrap_or(Value::Null);
        let record = record.clone();
        let state = info.get("state").cloned().unwrap_or(Value::Null);
        self.dispatch(
            "downloadProgress",
            json!({
                "guid": guid, "totalBytes": record.total_bytes, "receivedBytes": record.received_bytes,
                "state": state, "filePath": record.file_path, "targetId": record.target_id,
            }),
        );
        let finished = matches!(state.as_str(), Some("completed" | "canceled"));
        if finished && !already_terminal {
            self.remember_terminal(guid);
            self.downloads.remove(guid);
            self.terminal_downloads
                .insert(guid.to_owned(), record.clone());
            self.dispatch(
                "downloadFinished",
                json!({
                    "guid": guid, "totalBytes": record.total_bytes, "receivedBytes": record.received_bytes,
                    "state": state, "cdpState": state, "filePath": record.file_path,
                    "targetId": record.target_id, "error": null,
                }),
            );
        }
    }

    fn disconnect_downloads(&mut self) {
        let records: Vec<Download> = std::mem::take(&mut self.downloads).into_values().collect();
        for record in records {
            self.remember_terminal(&record.guid);
            self.dispatch(
                "downloadFinished",
                json!({
                    "guid": record.guid, "totalBytes": record.total_bytes,
                    "receivedBytes": record.received_bytes, "state": "failed", "cdpState": null,
                    "filePath": record.file_path, "targetId": record.target_id,
                    "error": {"code": "browserDisconnected", "message": "Browser transport disconnected"},
                }),
            );
        }
    }

    fn handle(&mut self, event: &Event) {
        let params = &event.params;
        let session = params
            .get("sessionId")
            .and_then(Value::as_str)
            .or(event.session_id.as_deref())
            .map(str::to_owned);
        match event.method.as_str() {
            "Target.targetCreated" => self.created(params, false),
            "Target.targetDestroyed" => {
                if let Some(id) = params.get("targetId").and_then(Value::as_str) {
                    self.destroyed(id);
                }
            }
            "Target.targetInfoChanged" => {
                if let Some(target) = params.get("targetInfo").and_then(page_target) {
                    let id = target["targetId"].as_str().unwrap_or_default().to_owned();
                    if self.targets.contains_key(&id) {
                        self.targets.insert(id, normalize_target(target));
                    }
                }
            }
            "Target.targetCrashed" => {
                if let Some(id) = params.get("targetId").and_then(Value::as_str) {
                    self.crashes.insert(
                        id.to_owned(),
                        json!({
                            "status": params.get("status").and_then(Value::as_str).unwrap_or_default(),
                            "errorCode": params.get("errorCode").and_then(Value::as_i64).unwrap_or(0),
                        }),
                    );
                }
            }
            "Target.attachedToTarget" => {
                if let (Some(target), Some(session)) = (
                    params
                        .pointer("/targetInfo/targetId")
                        .and_then(Value::as_str),
                    session,
                ) {
                    self.session_to_target.insert(session, target.to_owned());
                }
            }
            "Target.detachedFromTarget" => {
                if let Some(session) = session {
                    self.session_to_target.remove(&session);
                }
            }
            "Browser.downloadWillBegin" => self.download_begin(params),
            "Browser.downloadProgress" => self.download_progress(params),
            "Page.frameAttached" => {
                if let (Some(session), Some(frame)) =
                    (session, params.get("frameId").and_then(Value::as_str))
                {
                    self.map_frame(frame, &session);
                }
            }
            "Page.frameNavigated" => {
                if let (Some(session), Some(frame)) =
                    (session, params.pointer("/frame/id").and_then(Value::as_str))
                {
                    self.map_frame(frame, &session);
                }
            }
            _ => {}
        }
    }

    fn fold(&mut self) {
        if self.disposed {
            return;
        }
        let queued = self.events.as_ref().map(Subscription::drain);
        for event in queued.unwrap_or_default() {
            self.handle(&event);
        }
        if !self.disconnected && !self.client.is_connected() {
            self.disconnected = true;
            self.disconnect_downloads();
        }
    }

    /// Bridge events produced since the last call, oldest first.
    pub fn poll(&mut self) -> Vec<BrowserEvent> {
        self.fold();
        self.pending.drain(..).collect()
    }

    /// Open tabs as last reported, in discovery order.
    pub fn targets(&self) -> Vec<Value> {
        self.target_order
            .iter()
            .filter_map(|id| self.targets.get(id).cloned())
            .collect()
    }

    /// Waits for the next `name` event produced after this call that satisfies `predicate`.
    /// Earlier events are consumed and returned by the next [`Self::poll`].
    pub fn wait_for_event(
        &mut self,
        name: &str,
        timeout: Duration,
        predicate: impl Fn(&Value) -> bool,
    ) -> Result<Value, String> {
        if !BROWSER_EVENT_NAMES.contains(&name) {
            return Err(format!("Unknown browser event: {name}"));
        }
        if self.disposed {
            return Err("Events bridge is disposed".into());
        }
        self.fold();
        let mut skipped = self.pending.len();
        let deadline = Instant::now() + timeout;
        loop {
            self.fold();
            let found = self
                .pending
                .iter()
                .enumerate()
                .skip(skipped)
                .find(|(_, event)| event.name == name && predicate(&event.payload))
                .map(|(index, _)| index);
            if let Some(index) = found {
                return self
                    .pending
                    .remove(index)
                    .map(|event| event.payload)
                    .ok_or_else(|| "event vanished".into());
            }
            skipped = self.pending.len();
            if self.disconnected {
                return Err("Browser transport disconnected".into());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!("Timed out waiting for browser event \"{name}\""));
            }
            if let Some(events) = &self.events {
                events.wait(deadline - now);
            }
        }
    }

    /// Stops processing events.
    pub fn dispose(&mut self) {
        self.disposed = true;
        self.events = None;
        self.pending.clear();
    }

    /// Guid of every download still in progress.
    pub fn active_downloads(&self) -> HashSet<String> {
        self.downloads.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake_cdp::FakeCdp;
    use super::*;

    fn page(id: &str, extra: Value) -> Value {
        let mut info = json!({"targetId": id, "type": "page", "url": "about:blank", "title": "", "attached": false});
        if let (Some(info), Some(extra)) = (info.as_object_mut(), extra.as_object()) {
            info.extend(extra.clone());
        }
        info
    }

    fn fake(targets: Value) -> FakeCdp {
        FakeCdp::new(Box::new(move |method: &str, _: &Value, _: Option<&str>| {
            Ok(if method == "Target.getTargets" {
                json!({"targetInfos": targets.clone()})
            } else {
                json!({})
            })
        }))
    }

    fn named<'a>(events: &'a [BrowserEvent], name: &str) -> Vec<&'a Value> {
        events
            .iter()
            .filter(|event| event.name == name)
            .map(|event| &event.payload)
            .collect()
    }

    #[test]
    fn events_startup_filtering_popup_classification_close_and_waits() {
        let cdp =
            fake(json!([page("a", json!({})), {"targetId": "worker", "type": "service_worker"}]));
        let mut events = BrowserEvents::new(cdp.client.clone(), DownloadBehavior::Default)
            .unwrap_or_else(|error| panic!("{error}"));
        let startup = events.poll();
        let opened = named(&startup, "tabOpened");
        assert_eq!(opened.len(), 1);
        assert_eq!(opened[0]["initial"], true);
        let discover = cdp.calls_of("Target.setDiscoverTargets");
        assert_eq!(discover[0].params["discover"], true);
        assert_eq!(cdp.calls_of("Target.getTargets")[0].session, None);

        cdp.emit(
            "Target.targetCreated",
            json!({"targetInfo": page("b", json!({"url": "https://b.test", "openerId": "a", "openerFrameId": "frame-a"}))}),
            None,
        );
        cdp.flush();
        let created = events.poll();
        assert_eq!(named(&created, "tabOpened").len(), 1);
        let popups = named(&created, "popupOpened");
        assert_eq!(popups.len(), 1);
        assert_eq!(popups[0]["openerTargetId"], "a");

        cdp.emit(
            "Target.targetInfoChanged",
            json!({"targetInfo": page("b", json!({"url": "https://updated.test"}))}),
            None,
        );
        cdp.emit(
            "Target.targetCrashed",
            json!({"targetId": "b", "status": "crashed", "errorCode": 9}),
            None,
        );
        cdp.emit("Target.targetDestroyed", json!({"targetId": "b"}), None);
        cdp.emit("Target.targetDestroyed", json!({"targetId": "b"}), None);
        cdp.flush();
        let closed_events = events.poll();
        let closed = named(&closed_events, "tabClosed");
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0]["reason"], "crashed");
        assert_eq!(closed[0]["lastKnown"]["url"], "https://updated.test");

        let timed_out = events.wait_for_event("tabOpened", Duration::from_millis(1), |_| true);
        assert!(
            timed_out
                .err()
                .is_some_and(|error| error.contains("Timed out waiting"))
        );
        cdp.disconnect();
        let gone = events.wait_for_event("tabOpened", Duration::from_secs(5), |_| true);
        assert!(
            gone.err()
                .is_some_and(|error| error.contains("disconnected"))
        );
        events.dispose();
        events.dispose();
    }

    #[test]
    fn downloads_map_progress_and_fail_in_flight_work_on_disconnect() {
        let cdp = fake(json!([page("a", json!({}))]));
        let mut events = BrowserEvents::new(
            cdp.client.clone(),
            DownloadBehavior::Allow("/tmp/downloads".into()),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        events.poll();
        cdp.emit(
            "Target.attachedToTarget",
            json!({"targetInfo": {"targetId": "a"}}),
            Some("session-a"),
        );
        cdp.emit(
            "Page.frameAttached",
            json!({"frameId": "mapped"}),
            Some("session-a"),
        );
        cdp.emit(
            "Browser.downloadWillBegin",
            json!({"guid": "g", "url": "https://x/file", "suggestedFilename": "file", "frameId": "mapped"}),
            None,
        );
        cdp.emit(
            "Browser.downloadProgress",
            json!({"guid": "g", "totalBytes": 4, "receivedBytes": 2, "state": "inProgress"}),
            None,
        );
        cdp.emit(
            "Browser.downloadProgress",
            json!({"guid": "g", "totalBytes": 4, "receivedBytes": 4, "state": "completed", "filePath": "/tmp/downloads/file"}),
            None,
        );
        cdp.emit(
            "Browser.downloadProgress",
            json!({"guid": "g", "totalBytes": 4, "receivedBytes": 4, "state": "completed"}),
            None,
        );
        cdp.emit(
            "Browser.downloadWillBegin",
            json!({"guid": "pending", "url": "https://x/p", "suggestedFilename": "p", "frameId": "mapped"}),
            None,
        );
        cdp.flush();
        let seen = events.poll();
        let started = named(&seen, "downloadStarted");
        assert_eq!(started[0]["targetId"], "a");
        assert_eq!(started[1]["targetId"], "a");
        assert_eq!(named(&seen, "downloadProgress").len(), 3);
        let finished = named(&seen, "downloadFinished");
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0]["state"], "completed");
        assert_eq!(finished[0]["filePath"], "/tmp/downloads/file");

        cdp.disconnect();
        cdp.client.wait_event(0, Duration::from_secs(5), |_| false);
        let after = events.poll();
        let failed = named(&after, "downloadFinished");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["guid"], "pending");
        assert_eq!(failed[0]["state"], "failed");
        assert_eq!(failed[0]["error"]["code"], "browserDisconnected");
        assert!(events.poll().is_empty(), "a guid finishes once");

        let behavior = cdp.calls_of("Browser.setDownloadBehavior");
        assert_eq!(behavior.len(), 1);
        assert_eq!(behavior[0].session, None);
        assert_eq!(
            behavior[0].params,
            json!({"behavior": "allow", "eventsEnabled": true, "downloadPath": "/tmp/downloads"})
        );
    }

    #[test]
    fn unknown_events_and_relative_download_paths_are_rejected() {
        let cdp = fake(json!([]));
        assert!(
            BrowserEvents::new(
                cdp.client.clone(),
                DownloadBehavior::Allow("rel/dir".into())
            )
            .err()
            .is_some_and(|error| error.contains("absolute"))
        );
        let mut events = BrowserEvents::new(cdp.client.clone(), DownloadBehavior::Deny)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(
            events
                .wait_for_event("nope", Duration::from_millis(1), |_| true)
                .is_err()
        );
        assert!(path_is_absolute("C:\\downloads"));
    }
}
