//! Flight-recorder traces for one page.
//!
//! Port of omowright `src/trace.js` (`createTrace`): steps with before/after screenshots, network
//! (through [`NetworkSnoop`]), console, dialogs, main-frame navigations and downloads, flushed to
//! `dir` as `trace.jsonl`, `trace.har` and `summary.json` by [`Trace::stop`] (idempotent: the
//! second call returns the first summary and writes nothing).
//!
//! PORT-DIVERGENCE: the JS trace subscribes to `Page.frameNavigated` and the page's `dialog`
//! emitter. Here a [`Subscription`] queues both for the page session without loss and the queue
//! is drained after each step and at stop; download events come from a [`BrowserEvents`] bridge the
//! caller hands to [`Trace::observe`] (the JS `events` option).
use super::events::BrowserEvent;
use super::har::{HarOptions, to_har};
use super::network_snoop::{NetworkEntry, NetworkSnoop, SnoopOptions, now_ms};
use crate::cdp::{CdpClient, Subscription};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const SLUG_MAX: usize = 48;
const SNOOP_MAX_ENTRIES: usize = 5000;

/// What a trace needs from a page.
pub trait TracePage {
    fn cdp(&self) -> &CdpClient;
    fn session(&self) -> &str;
    /// PNG of the viewport.
    fn screenshot_png(&mut self) -> Result<Vec<u8>, String>;
    /// Every console entry buffered in the page so far (oldest first).
    fn console_entries(&mut self) -> Result<Vec<Value>, String>;
    fn current_url(&mut self) -> String;
}

/// `createTrace` options.
#[derive(Clone, Debug)]
pub struct TraceOptions {
    pub dir: PathBuf,
    pub screenshots: bool,
    pub network: bool,
    pub console: bool,
    pub max_body_bytes: u64,
}

impl TraceOptions {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            screenshots: true,
            network: true,
            console: true,
            max_body_bytes: 1_000_000,
        }
    }
}

/// omowright `slug`: lowercase, non-alphanumerics to `-`, trimmed, at most 48 characters.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "step".into()
    } else {
        trimmed.chars().take(SLUG_MAX).collect()
    }
}

/// A running trace.
pub struct Trace {
    options: TraceOptions,
    shot_dir: PathBuf,
    started_at: i64,
    records: Vec<Value>,
    net_entries: Vec<NetworkEntry>,
    snoop: Option<NetworkSnoop>,
    page_events: Subscription,
    seq: u64,
    console_seen: usize,
    summary: Option<Value>,
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    base
}

impl Trace {
    /// Starts recording `page` into `options.dir`.
    pub fn new(page: &impl TracePage, options: TraceOptions) -> Result<Self, String> {
        if options.dir.as_os_str().is_empty() {
            return Err("createTrace requires a dir".into());
        }
        let shot_dir = options.dir.join("screenshots");
        std::fs::create_dir_all(&shot_dir)
            .map_err(|error| format!("cannot create {}: {error}", shot_dir.display()))?;
        let snoop = options.network.then(|| {
            NetworkSnoop::new(
                page.cdp().clone(),
                page.session(),
                SnoopOptions {
                    max_body_bytes: options.max_body_bytes,
                    max_entries: SNOOP_MAX_ENTRIES,
                    ..SnoopOptions::default()
                },
            )
        });
        Ok(Self {
            shot_dir,
            started_at: now_ms(),
            records: Vec::new(),
            net_entries: Vec::new(),
            snoop,
            page_events: page.cdp().subscribe({
                let session = page.session().to_owned();
                move |event: &crate::cdp::Event| {
                    event.session_id.as_deref() == Some(session.as_str())
                        && matches!(
                            event.method.as_str(),
                            "Page.frameNavigated" | "Page.javascriptDialogOpening"
                        )
                }
            }),
            seq: 0,
            console_seen: 0,
            summary: None,
            options,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.options.dir
    }

    fn record(&mut self, event: Value) -> usize {
        let entry = merge(event, json!({"ts": now_ms(), "seq": self.seq}));
        self.seq += 1;
        self.records.push(entry);
        self.records.len() - 1
    }

    /// Records the download events of a browser events bridge (the JS `events` option).
    pub fn observe(&mut self, events: &[BrowserEvent]) {
        if self.summary.is_some() {
            return;
        }
        for event in events {
            let phase = match event.name {
                "downloadStarted" => "started",
                "downloadFinished" => "finished",
                _ => continue,
            };
            self.record(merge(
                json!({"kind": "download", "phase": phase}),
                event.payload.clone(),
            ));
        }
    }

    fn drain_page_events(&mut self) {
        for event in self.page_events.drain() {
            let params = &event.params;
            match event.method.as_str() {
                "Page.frameNavigated" if params.pointer("/frame/parentId").is_none() => {
                    self.record(json!({
                        "kind": "navigation",
                        "url": params.pointer("/frame/url").and_then(Value::as_str).unwrap_or_default(),
                        "frameId": params.pointer("/frame/id").cloned().unwrap_or(Value::Null),
                    }));
                }
                "Page.javascriptDialogOpening" => {
                    self.record(json!({
                        "kind": "dialog",
                        "type": params.get("type").cloned().unwrap_or(Value::Null),
                        "message": params.get("message").and_then(Value::as_str).unwrap_or_default(),
                        "defaultPrompt": params.get("defaultPrompt").and_then(Value::as_str).unwrap_or_default(),
                        "url": params.get("url").cloned().unwrap_or(Value::Null),
                    }));
                }
                _ => {}
            }
        }
    }

    fn drain_network(&mut self) {
        let entries = self
            .snoop
            .as_mut()
            .map(NetworkSnoop::pop)
            .unwrap_or_default();
        for mut entry in entries {
            if entry
                .body
                .as_ref()
                .is_some_and(|body| body.len() as u64 > self.options.max_body_bytes)
            {
                entry.body = None;
                entry.body_skipped = true;
            }
            self.record(merge(json!({"kind": "network"}), entry.to_json()));
            self.net_entries.push(entry);
        }
    }

    fn drain_console(&mut self, page: &mut impl TracePage) {
        if !self.options.console {
            return;
        }
        // A closed or navigating page cannot report its console; keep the rest of the trace.
        let Ok(entries) = page.console_entries() else {
            return;
        };
        if entries.len() < self.console_seen {
            self.console_seen = 0;
        }
        for entry in entries.iter().skip(self.console_seen) {
            self.record(merge(json!({"kind": "console"}), entry.clone()));
        }
        self.console_seen = entries.len();
    }

    fn drain(&mut self, page: &mut impl TracePage) {
        self.drain_page_events();
        self.drain_network();
        self.drain_console(page);
    }

    fn capture(&mut self, page: &mut impl TracePage, step: usize, phase: &str) {
        let seq = self.records[step]["seq"].as_u64().unwrap_or_default();
        let name = self.records[step]["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let file = self
            .shot_dir
            .join(format!("{seq}-{}-{phase}.png", slug(&name)));
        let written = page.screenshot_png().and_then(|png| {
            std::fs::write(&file, png).map_err(|error| format!("{}: {error}", file.display()))
        });
        match written {
            Ok(()) => {
                self.records[step]["screenshots"][phase] = json!(file.display().to_string());
            }
            Err(message) => {
                self.record(json!({"kind": "warning", "scope": format!("screenshot:{phase}"), "step": seq, "message": message}));
            }
        }
    }

    /// Runs `body` as a named step: screenshots before and after, its error recorded and
    /// returned, then network, console and page events drained.
    pub fn step<P: TracePage, T>(
        &mut self,
        page: &mut P,
        name: &str,
        body: impl FnOnce(&mut P) -> Result<T, String>,
    ) -> Result<T, String> {
        // Events that happened before the step belong before it in the trace.
        self.drain_page_events();
        let step = self.record(
            json!({"kind": "step", "name": name, "startedAt": now_ms(), "screenshots": {}}),
        );
        if self.options.screenshots {
            self.capture(&mut *page, step, "before");
        }
        let outcome = body(&mut *page);
        if let Err(error) = &outcome {
            self.records[step]["error"] = json!(error);
        }
        let ended = now_ms();
        let started = self.records[step]["startedAt"].as_i64().unwrap_or(ended);
        self.records[step]["endedAt"] = json!(ended);
        self.records[step]["durationMs"] = json!(ended - started);
        if self.options.screenshots {
            self.capture(&mut *page, step, "after");
        }
        self.drain(page);
        outcome
    }

    /// Records a named mark with optional data.
    pub fn mark(&mut self, name: &str, data: Option<Value>) -> Value {
        let index = self.record(json!({"kind": "mark", "name": name, "data": data}));
        self.records[index].clone()
    }

    /// Drains what is left, writes the three files and returns the summary. Later calls return
    /// the same summary without writing.
    pub fn stop(&mut self, page: &mut impl TracePage) -> Result<Value, String> {
        if let Some(summary) = &self.summary {
            return Ok(summary.clone());
        }
        self.drain(page);
        if let Some(snoop) = self.snoop.as_mut() {
            snoop.dispose();
        }
        let summary = json!({
            "steps": self.records.iter().filter(|entry| entry["kind"] == "step").count(),
            "events": self.records.len(),
            "network": self.net_entries.len(),
            "startedAt": self.started_at,
            "endedAt": now_ms(),
            "url": page.current_url(),
        });
        self.summary = Some(summary.clone());
        let dir = &self.options.dir;
        let write = |name: &str, text: String| {
            std::fs::write(dir.join(name), text)
                .map_err(|error| format!("cannot write {name}: {error}"))
        };
        let lines: String = self
            .records
            .iter()
            .map(|entry| format!("{entry}\n"))
            .collect();
        write("trace.jsonl", lines)?;
        let har = to_har(&self.net_entries, &HarOptions::default());
        write(
            "trace.har",
            format!(
                "{}\n",
                serde_json::to_string_pretty(&har).unwrap_or_default()
            ),
        )?;
        write(
            "summary.json",
            format!(
                "{}\n",
                serde_json::to_string_pretty(&summary).unwrap_or_default()
            ),
        )?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake_cdp::FakeCdp;
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

    struct FakePage {
        cdp: FakeCdp,
        console: Vec<Value>,
        shots: usize,
        shot_error: Option<String>,
    }

    impl TracePage for FakePage {
        fn cdp(&self) -> &CdpClient {
            &self.cdp.client
        }
        fn session(&self) -> &str {
            "s1"
        }
        fn screenshot_png(&mut self) -> Result<Vec<u8>, String> {
            self.shots += 1;
            match &self.shot_error {
                Some(error) => Err(error.clone()),
                None => Ok(PNG.to_vec()),
            }
        }
        fn console_entries(&mut self) -> Result<Vec<Value>, String> {
            Ok(self.console.clone())
        }
        fn current_url(&mut self) -> String {
            "https://x.test/app".into()
        }
    }

    fn page() -> FakePage {
        let cdp = FakeCdp::new(Box::new(|method: &str, _: &Value, _: Option<&str>| {
            Ok(if method == "Network.getResponseBody" {
                json!({"body": "{\"ok\":true}", "base64Encoded": false})
            } else {
                json!({})
            })
        }));
        FakePage {
            cdp,
            console: Vec::new(),
            shots: 0,
            shot_error: None,
        }
    }

    fn round_trip(cdp: &FakeCdp, id: &str) {
        cdp.emit("Network.requestWillBeSent", json!({"requestId": id, "request": {"url": "https://x.test/api/items?page=2", "method": "GET", "headers": {"accept": "*/*"}}, "type": "XHR", "wallTime": 1_700_000_000}), Some("s1"));
        cdp.emit("Network.responseReceived", json!({"requestId": id, "type": "XHR", "response": {"status": 200, "statusText": "OK", "mimeType": "application/json", "headers": {"content-type": "application/json"}}}), Some("s1"));
        cdp.emit(
            "Network.loadingFinished",
            json!({"requestId": id, "encodedDataLength": 11}),
            Some("s1"),
        );
        cdp.flush();
    }

    fn lines(dir: &Path) -> Vec<Value> {
        std::fs::read_to_string(dir.join("trace.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn shots(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.join("screenshots"))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn a_trace_run_writes_jsonl_har_screenshots_and_a_summary() {
        let dir = tempdir();
        let mut page = page();
        let mut trace = Trace::new(&page, TraceOptions::new(dir.path()))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(trace.dir(), dir.path());
        page.cdp.emit("Page.javascriptDialogOpening", json!({"type": "confirm", "message": "sure?", "defaultPrompt": "", "url": "https://x.test/app"}), Some("s1"));
        page.cdp.emit(
            "Page.frameNavigated",
            json!({"frame": {"id": "F1", "url": "https://x.test/app"}}),
            Some("s1"),
        );
        page.cdp.emit(
            "Page.frameNavigated",
            json!({"frame": {"id": "F2", "parentId": "F1", "url": "https://x.test/iframe"}}),
            Some("s1"),
        );
        page.cdp.emit(
            "Page.frameNavigated",
            json!({"frame": {"id": "F3", "url": "https://other.test/"}}),
            Some("other"),
        );
        page.console = vec![
            json!({"level": "log", "message": "hello", "timestamp": "2024-01-01T00:00:00.000Z"}),
        ];
        round_trip(&page.cdp, "r1");
        let result = trace.step(&mut page, "Click Sign In", |_| Ok("done"));
        assert_eq!(result, Ok("done"));
        trace.mark("checkpoint", Some(json!({"n": 1})));
        let summary = trace
            .stop(&mut page)
            .unwrap_or_else(|error| panic!("{error}"));

        let lines = lines(dir.path());
        let seqs: Vec<u64> = lines.iter().filter_map(|l| l["seq"].as_u64()).collect();
        assert_eq!(seqs, (0..lines.len() as u64).collect::<Vec<_>>());
        assert!(lines.iter().all(|line| line["ts"].is_number()));
        let kinds: Vec<&str> = lines.iter().filter_map(|l| l["kind"].as_str()).collect();
        for kind in ["step", "network", "dialog", "navigation", "console", "mark"] {
            assert!(kinds.contains(&kind), "missing {kind}: {kinds:?}");
        }
        assert_eq!(
            kinds.iter().filter(|kind| **kind == "navigation").count(),
            1,
            "child frames and other sessions are ignored"
        );
        let find = |kind: &str| {
            lines
                .iter()
                .find(|line| line["kind"] == kind)
                .cloned()
                .unwrap_or_default()
        };
        let step = find("step");
        assert_eq!(step["name"], "Click Sign In");
        assert!(step["endedAt"].as_i64() >= step["startedAt"].as_i64());
        assert_eq!(
            step["durationMs"].as_i64(),
            Some(step["endedAt"].as_i64().unwrap_or(0) - step["startedAt"].as_i64().unwrap_or(0))
        );
        assert!(step.get("error").is_none());
        let dialog = find("dialog");
        assert_eq!(dialog["type"], "confirm");
        assert_eq!(dialog["message"], "sure?");
        assert_eq!(dialog["url"], "https://x.test/app");
        assert_eq!(find("navigation")["frameId"], "F1");
        assert_eq!(find("console")["message"], "hello");
        assert_eq!(find("mark")["data"], json!({"n": 1}));
        let network = find("network");
        assert_eq!(network["url"], "https://x.test/api/items?page=2");
        assert_eq!(network["body"], "{\"ok\":true}");

        let seq = step["seq"].as_u64().unwrap_or_default();
        assert_eq!(
            shots(dir.path()),
            [
                format!("{seq}-click-sign-in-after.png"),
                format!("{seq}-click-sign-in-before.png")
            ]
        );
        assert_eq!(page.shots, 2);
        let har: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("trace.har")).unwrap_or_default(),
        )
        .unwrap_or_default();
        assert_eq!(har["log"]["version"], "1.2");
        assert_eq!(
            har["log"]["entries"][0]["request"]["url"],
            "https://x.test/api/items?page=2"
        );
        let on_disk: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("summary.json")).unwrap_or_default(),
        )
        .unwrap_or_default();
        assert_eq!(summary, on_disk);
        assert_eq!(summary["steps"], 1);
        assert_eq!(summary["network"], 1);
        assert_eq!(summary["events"], lines.len());
        assert_eq!(summary["url"], "https://x.test/app");
    }

    #[test]
    fn navigations_older_than_the_client_event_log_are_still_traced() {
        let dir = tempdir();
        let mut page = page();
        let mut trace = Trace::new(&page, TraceOptions::new(dir.path()))
            .unwrap_or_else(|error| panic!("{error}"));
        page.cdp.emit(
            "Page.frameNavigated",
            json!({"frame": {"id": "F1", "url": "https://x.test/first"}}),
            Some("s1"),
        );
        for index in 0..5000 {
            page.cdp.emit(
                "Network.dataReceived",
                json!({"requestId": format!("n{index}")}),
                Some("other"),
            );
        }
        round_trip(&page.cdp, "r1");
        let _ = trace.step(&mut page, "after the burst", |_| Ok(()));
        trace
            .stop(&mut page)
            .unwrap_or_else(|error| panic!("{error}"));
        let lines = lines(dir.path());
        assert!(
            lines
                .iter()
                .any(|line| line["kind"] == "navigation" && line["url"] == "https://x.test/first")
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line["kind"] == "network")
                .count(),
            1
        );
    }

    #[test]
    fn a_failing_step_is_recorded_with_its_error_and_returned() {
        let dir = tempdir();
        let mut page = page();
        let mut options = TraceOptions::new(dir.path());
        options.network = false;
        options.console = false;
        let mut trace = Trace::new(&page, options).unwrap_or_else(|error| panic!("{error}"));
        let outcome: Result<(), String> =
            trace.step(&mut page, "boom", |_| Err("step exploded".into()));
        assert_eq!(outcome, Err("step exploded".into()));
        let _ = trace.stop(&mut page);
        let step = lines(dir.path())
            .into_iter()
            .find(|line| line["kind"] == "step")
            .unwrap_or_default();
        assert_eq!(step["error"], "step exploded");
        assert!(step["durationMs"].is_number());
        assert_eq!(
            shots(dir.path()).len(),
            2,
            "the after screenshot is still taken"
        );
    }

    #[test]
    fn screenshot_failures_become_warnings_and_never_break_the_step() {
        let dir = tempdir();
        let mut page = page();
        page.shot_error = Some("capture timed out".into());
        let mut options = TraceOptions::new(dir.path());
        options.network = false;
        options.console = false;
        let mut trace = Trace::new(&page, options).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(trace.step(&mut page, "shot", |_| Ok(42)), Ok(42));
        let _ = trace.stop(&mut page);
        let warnings: Vec<Value> = lines(dir.path())
            .into_iter()
            .filter(|line| line["kind"] == "warning")
            .collect();
        assert_eq!(warnings.len(), 2);
        assert!(
            warnings[0]["message"]
                .as_str()
                .is_some_and(|m| m.contains("capture timed out"))
        );
        assert!(shots(dir.path()).is_empty());
    }

    #[test]
    fn screenshots_false_skips_capture_and_stop_is_idempotent() {
        let dir = tempdir();
        let mut page = page();
        let mut options = TraceOptions::new(dir.path());
        options.screenshots = false;
        let mut trace = Trace::new(&page, options).unwrap_or_else(|error| panic!("{error}"));
        let _ = trace.step(&mut page, "no shots", |_| Ok(()));
        assert_eq!(page.shots, 0);
        assert!(shots(dir.path()).is_empty());
        let first = trace.stop(&mut page);
        assert!(dir.path().join("trace.jsonl").exists());
        let _ = std::fs::write(dir.path().join("trace.jsonl"), "SENTINEL\n");
        let second = trace.stop(&mut page);
        assert_eq!(second, first, "stop() returns the same summary");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("trace.jsonl")).ok(),
            Some("SENTINEL\n".into()),
            "stop() writes once"
        );
    }

    #[test]
    fn download_events_from_the_bridge_are_recorded() {
        let dir = tempdir();
        let mut page = page();
        let mut trace =
            Trace::new(&page, TraceOptions::new(dir.path())).unwrap_or_else(|e| panic!("{e}"));
        trace.observe(&[
            BrowserEvent {
                name: "downloadStarted",
                payload: json!({"guid": "g"}),
            },
            BrowserEvent {
                name: "downloadProgress",
                payload: json!({"guid": "g"}),
            },
            BrowserEvent {
                name: "downloadFinished",
                payload: json!({"guid": "g", "state": "completed"}),
            },
        ]);
        let _ = trace.stop(&mut page);
        let downloads: Vec<Value> = lines(dir.path())
            .into_iter()
            .filter(|line| line["kind"] == "download")
            .collect();
        assert_eq!(downloads.len(), 2);
        assert_eq!(downloads[0]["phase"], "started");
        assert_eq!(downloads[1]["phase"], "finished");
        assert_eq!(downloads[1]["state"], "completed");
    }

    #[test]
    fn slugs_follow_upstream() {
        assert_eq!(slug("Click Sign In"), "click-sign-in");
        assert_eq!(slug("  !!  "), "step");
        assert_eq!(slug(&"a".repeat(80)).len(), 48);
    }
}
