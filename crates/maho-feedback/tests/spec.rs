//! Tests written from docs/cleanroom/specs/feedback.md, section 8 (acceptance tests 1-15).

use jiff::{SignedDuration, Timestamp};
use maho_ext_api::{NotificationType, SessionEntry};
use maho_feedback::client::ClientIdentity;
use maho_feedback::collect::{
    SessionHeader, collect_logs, config_text, render_session, scrub_config, shorten_body,
};
use maho_feedback::flow::{
    self, CANCEL, FlowBackend, FlowFuture, FlowUi, ReportsBackend, SEND, VIEW,
};
use maho_feedback::reports::{TransportFuture, build_sections};
use maho_feedback::{
    Destination, Host, HttpRequest, HttpResponse, LiveSession, PrepareOutcome, PrepareRequest,
    Redacted, Refusal, ReportKind, Reports, SectionId, Store, SubmitResult,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------------------------
// Fixtures

const SECRET: &str = "SECRET";
const MASK: &str = "[masked]";

/// Replaces every `SECRET` with `[masked]`; running it again changes nothing.
fn redact(text: &str) -> Redacted {
    Redacted {
        text: text.replace(SECRET, MASK),
        count: text.matches(SECRET).count(),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(future)
}

fn ts(text: &str) -> Timestamp {
    text.parse().expect("timestamp")
}

#[derive(Default)]
struct FakeIntake {
    requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<VecDeque<Result<HttpResponse, String>>>,
}

impl FakeIntake {
    fn respond(&self, status: u16, body: &str) {
        self.responses
            .lock()
            .expect("lock")
            .push_back(Ok(HttpResponse {
                status,
                body: body.into(),
            }));
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    agent_dir: PathBuf,
    env: Arc<Mutex<BTreeMap<String, String>>>,
    now: Arc<Mutex<Timestamp>>,
    intake: Arc<FakeIntake>,
    config: Arc<Mutex<Value>>,
    store: Arc<Store>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp");
        let agent_dir = dir.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        Self {
            _dir: dir,
            agent_dir,
            env: Arc::new(Mutex::new(BTreeMap::new())),
            now: Arc::new(Mutex::new(ts("2025-06-07T22:30:00Z"))),
            intake: Arc::new(FakeIntake::default()),
            config: Arc::new(Mutex::new(json!({"theme": "dark"}))),
            store: Arc::new(Store::default()),
        }
    }

    fn set_env(&self, key: &str, value: &str) {
        self.env
            .lock()
            .expect("lock")
            .insert(key.into(), value.into());
    }

    fn advance(&self, by: SignedDuration) {
        let mut now = self.now.lock().expect("lock");
        *now = now.checked_add(by).expect("time");
    }

    fn host(&self, with_redactor: bool) -> Host {
        let env = Arc::clone(&self.env);
        let now = Arc::clone(&self.now);
        let intake = Arc::clone(&self.intake);
        let config = Arc::clone(&self.config);
        Host {
            redactor: with_redactor.then(|| Arc::new(redact) as maho_feedback::reports::Redactor),
            transport: Arc::new(move |request: HttpRequest| {
                let intake = Arc::clone(&intake);
                let future: TransportFuture = Box::pin(async move {
                    intake.requests.lock().expect("lock").push(request);
                    intake
                        .responses
                        .lock()
                        .expect("lock")
                        .pop_front()
                        .unwrap_or_else(|| Err("no scripted response".into()))
                });
                future
            }),
            env: Arc::new(move |key: &str| env.lock().expect("lock").get(key).cloned()),
            clock: Arc::new(move || *now.lock().expect("lock")),
            config: Arc::new(move |_cwd: &Path| config.lock().expect("lock").clone()),
            agent_dir: self.agent_dir.clone(),
            default_cwd: PathBuf::from("/work"),
            product_version: "1.2.3".into(),
            host_version: "0.9.0".into(),
            host_name: "devbox".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            kernel: "6.1.0".into(),
        }
    }

    fn reports(&self) -> Reports {
        Reports::new(self.host(true), Arc::clone(&self.store))
    }

    fn reports_without_redactor(&self) -> Reports {
        Reports::new(self.host(false), Arc::clone(&self.store))
    }
}

fn draft_of(outcome: PrepareOutcome) -> maho_feedback::Draft {
    match outcome {
        PrepareOutcome::Draft(draft) => *draft,
        PrepareOutcome::Unavailable { reason } => panic!("unavailable: {reason}"),
    }
}

fn entry(kind: &str, timestamp: &str, data: Value) -> SessionEntry {
    SessionEntry {
        id: uuid_like(),
        parent_id: None,
        timestamp: timestamp.into(),
        kind: kind.into(),
        data,
    }
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!("e{}", NEXT.fetch_add(1, Ordering::SeqCst))
}

fn message_entry(timestamp: &str, message: Value) -> SessionEntry {
    entry(
        "message",
        timestamp,
        json!({"type": "message", "timestamp": timestamp, "message": message}),
    )
}

fn live(id: &str, entries: Vec<SessionEntry>) -> LiveSession {
    LiveSession {
        session_id: id.into(),
        cwd: PathBuf::from(format!("/work/{id}")),
        model: Some("acme/model-x".into()),
        entries: Arc::new(move || entries.clone()),
    }
}

/// Records dialogs and notifications; answers inputs and selections from scripts.
#[derive(Default)]
struct FakeUi {
    inputs: Mutex<VecDeque<Option<String>>>,
    selections: Mutex<VecDeque<Option<String>>>,
    asked_input: Mutex<usize>,
    titles: Mutex<Vec<String>>,
    notices: Mutex<Vec<(String, NotificationType)>>,
}

impl FakeUi {
    fn notices(&self) -> Vec<(String, NotificationType)> {
        self.notices.lock().expect("lock").clone()
    }
}

impl FlowUi for FakeUi {
    fn input<'a>(&'a self, _title: &'a str) -> FlowFuture<'a, Option<String>> {
        *self.asked_input.lock().expect("lock") += 1;
        let answer = self.inputs.lock().expect("lock").pop_front().flatten();
        Box::pin(async move { answer })
    }

    fn select<'a>(
        &'a self,
        title: &'a str,
        _options: &'a [String],
    ) -> FlowFuture<'a, Option<String>> {
        self.titles.lock().expect("lock").push(title.into());
        let answer = self.selections.lock().expect("lock").pop_front().flatten();
        Box::pin(async move { answer })
    }

    fn notify(&self, message: &str, kind: NotificationType) {
        self.notices
            .lock()
            .expect("lock")
            .push((message.into(), kind));
    }
}

/// A backend over a fixed prepare outcome that records submits.
struct FakeBackend {
    prepare: Result<PrepareOutcome, String>,
    submits: Mutex<Vec<Value>>,
    result: SubmitResult,
}

impl FlowBackend for FakeBackend {
    fn prepare(&self) -> Result<PrepareOutcome, String> {
        self.prepare.clone()
    }

    fn submit(&self, input: Value) -> FlowFuture<'_, SubmitResult> {
        self.submits.lock().expect("lock").push(input);
        let result = self.result.clone();
        Box::pin(async move { result })
    }
}

fn section_ids(draft: &Value) -> Vec<String> {
    draft["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .map(|section| section["id"].as_str().expect("id").to_owned())
        .collect()
}

fn section_text<'a>(draft: &'a Value, id: &str) -> Option<&'a str> {
    draft["sections"]
        .as_array()?
        .iter()
        .find(|section| section["id"] == id)?
        .get("text")?
        .as_str()
}

// ---------------------------------------------------------------------------------------------
// 1

#[test]
fn t01_without_a_redactor_nothing_is_collected_or_sent() {
    let fx = Fixture::new();
    let reports = fx.reports_without_redactor();
    let ui = FakeUi::default();
    let backend = ReportsBackend {
        reports: reports.clone(),
        request: PrepareRequest::default(),
    };
    block_on(flow::run(ReportKind::Bug, "it broke", &ui, &backend));
    let notices = ui.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].1, NotificationType::Warning);
    assert_eq!(*ui.asked_input.lock().expect("lock"), 0);
    assert!(ui.titles.lock().expect("lock").is_empty());

    let prepared = reports.prepare_json(&json!({}));
    assert_eq!(prepared["kind"], "unavailable");
    assert!(!prepared["reason"].as_str().expect("reason").is_empty());
    assert_eq!(fx.store.draft_count(), 0);

    let submitted = block_on(reports.submit_json(&json!({"draftId": "drf_x", "description": "d"})));
    assert_eq!(
        submitted,
        json!({"kind": "refused", "reason": "unavailable"})
    );
    assert!(fx.intake.requests().is_empty());
}

// ---------------------------------------------------------------------------------------------
// 2

#[test]
fn t02_sections_are_ordered_redacted_and_summed() {
    let host_sections = vec![
        (SectionId::Config, format!("{{\"k\": \"{SECRET}\"}}")),
        (SectionId::Logs, format!("{SECRET} {SECRET}")),
        (SectionId::Identity, format!("Host name: {SECRET}-box")),
        (SectionId::Environment, "Product version: 1.2.3".to_owned()),
    ];
    let sections = build_sections(host_sections, Vec::new(), &redact);
    let ids: Vec<SectionId> = sections.iter().map(|section| section.id).collect();
    assert_eq!(
        ids,
        [
            SectionId::Description,
            SectionId::Identity,
            SectionId::Environment,
            SectionId::Logs,
            SectionId::Config
        ]
    );
    assert_eq!(sections[0].text, "");
    assert_eq!(sections[1].text, format!("Host name: {SECRET}-box"));
    assert_eq!(sections[1].redacted_count, 0);
    assert_eq!(sections[3].text, format!("{MASK} {MASK}"));
    assert_eq!(sections[3].redacted_count, 2);
    assert_eq!(sections[4].redacted_count, 1);

    let fx = Fixture::new();
    *fx.config.lock().expect("lock") = json!({"note": SECRET, "theme": "dark"});
    let draft = fx.reports().prepare_json(&json!({"sessionId": null}));
    assert_eq!(draft["retentionDays"], 90);
    assert_eq!(draft["destination"], "intake");
    assert!(draft["draftId"].as_str().expect("id").starts_with("drf_"));
    assert_eq!(
        section_ids(&draft),
        ["description", "identity", "environment", "config"]
    );
    let sections = draft["sections"].as_array().expect("sections");
    let bytes: u64 = sections
        .iter()
        .map(|s| s["bytes"].as_u64().expect("bytes"))
        .sum();
    let count: u64 = sections
        .iter()
        .map(|s| s["redactedCount"].as_u64().expect("count"))
        .sum();
    assert_eq!(draft["totalBytes"].as_u64(), Some(bytes));
    assert_eq!(draft["redactedCount"].as_u64(), Some(count));
    assert_eq!(count, 1);
    for section in sections {
        assert_eq!(
            section["bytes"].as_u64(),
            Some(section["text"].as_str().expect("text").len() as u64)
        );
        assert!(!section["title"].as_str().expect("title").is_empty());
    }
    assert!(
        section_text(&draft, "identity")
            .expect("identity")
            .contains("devbox")
    );
    let environment = section_text(&draft, "environment").expect("environment");
    for fact in [
        "1.2.3",
        "0.9.0",
        "linux",
        "6.1.0",
        "x86_64",
        "latest",
        "2025-06-07T22:30:00.000Z",
    ] {
        assert!(
            environment.contains(fact),
            "{fact} missing from {environment}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 3

#[test]
fn t03_config_pass_blanks_secrets_and_keeps_ordinary_values() {
    let config = json!({
        "githubToken": "ghp_abc",
        "tokenLimit": 4096,
        "max_tokens": 1000,
        "theme": "dark",
        "mcp": {
            "Env": {"A": "1", "nested": {"B": "2"}},
            "headers": {"auth": {"inner": "bearer-x"}, "plain": "v"},
            "url": "ftp://alice:x@srv/a"
        },
        "providers": [{"name": "p", "options": {"accessKey": "AK"}}]
    });
    let out = scrub_config(&config);
    assert_eq!(out["githubToken"], "<redacted>");
    assert_eq!(out["tokenLimit"], 4096);
    assert_eq!(out["max_tokens"], 1000);
    assert_eq!(out["theme"], "dark");
    assert_eq!(
        out["mcp"]["Env"],
        json!({"A": "<redacted>", "nested": {"B": "<redacted>"}})
    );
    assert_eq!(out["mcp"]["headers"]["auth"]["inner"], "<redacted>");
    assert_eq!(out["mcp"]["headers"]["plain"], "<redacted>");
    assert_eq!(out["mcp"]["url"], "ftp://<redacted>@srv/a");
    assert_eq!(out["providers"][0]["options"]["accessKey"], "<redacted>");
    assert_eq!(out["providers"][0]["name"], "p");
    let text = config_text(&config);
    assert!(text.contains('\n'), "pretty-printed");
    assert!(!text.contains("ghp_abc"));
}

// ---------------------------------------------------------------------------------------------
// 4

#[test]
fn t04_transcript_renders_messages_only() {
    let entries = vec![
        entry(
            "custom",
            "2025-06-07T22:15:00Z",
            json!({"type": "custom", "data": {"x": 1}}),
        ),
        message_entry(
            "2025-06-07T22:15:09Z",
            json!({"role": "user", "content": "one\n\ntwo"}),
        ),
        message_entry(
            "2025-06-07T22:15:10Z",
            json!({"role": "assistant", "content": [
                {"type": "toolCall", "id": "c1", "name": "grep", "arguments": {"q": 1}},
                {"type": "text", "text": "done"}
            ]}),
        ),
        message_entry(
            "",
            json!({"role": "user", "content": [{"type": "image", "data": "", "mimeType": "image/png"}]}),
        ),
    ];
    let header = SessionHeader {
        session_id: Some("s1".into()),
        model: Some("acme/model-x".into()),
        cwd: None,
    };
    let text = render_session(&header, &entries);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains("s1"));
    assert!(lines[1].contains("acme/model-x"));
    assert!(lines[2].contains("unknown"));
    let transcript: Vec<&str> = lines.iter().skip(4).copied().collect();
    assert_eq!(
        transcript,
        [
            "user (22:15:09): one",
            "  two",
            "assistant (22:15:10): [grep] {\"q\":1} done",
            "user: [image]",
        ]
    );

    let many: Vec<SessionEntry> = (0..75)
        .map(|i| {
            message_entry(
                "2025-06-07T22:15:09Z",
                json!({"role": "user", "content": format!("m{i}")}),
            )
        })
        .collect();
    let text = render_session(&SessionHeader::default(), &many);
    let messages: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("user"))
        .collect();
    assert_eq!(messages.len(), 50);
    assert!(messages[0].ends_with(": m25"));
    assert!(messages[49].ends_with(": m74"));
}

// ---------------------------------------------------------------------------------------------
// 5

#[test]
fn t05_long_bodies_keep_both_ends() {
    let body: String = (0..4000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    let short = shorten_body(&body);
    let head: String = body.chars().take(1200).collect();
    let tail: String = body.chars().skip(2800).collect();
    assert!(short.starts_with(&head));
    assert!(short.ends_with(&tail));
    assert!(short.contains("1600"));
    assert!(short.chars().count() < 4000);

    let exact = "x".repeat(2400);
    assert_eq!(shorten_body(&exact), exact);
}

// ---------------------------------------------------------------------------------------------
// 6

#[test]
fn t06_logs_keep_only_recent_allowlisted_scalars() {
    let dir = tempfile::tempdir().expect("temp");
    let now = ts("2025-06-07T22:30:00Z");
    assert_eq!(collect_logs(&dir.path().join("logs"), now), None);

    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    let recent = json!({
        "ts": "2025-06-07T22:25:00Z",
        "level": "error",
        "event": "provider_retry",
        "attempt": 2,
        "durationMs": 120,
        "msg": "user said something private",
        "status": "a\nb",
        "path": "/home/me/secret.txt"
    });
    let lines = [
        recent.to_string(),
        json!({"ts": "2025-06-07T22:10:00Z", "level": "info", "event": "old"}).to_string(),
        json!({"level": "info", "event": "untimed"}).to_string(),
        "not json at all".to_owned(),
        "[1,2,3]".to_owned(),
    ];
    std::fs::write(logs.join("b.log"), lines.join("\n")).expect("write");
    std::fs::write(logs.join("a.log.old"), format!("{recent}\n")).expect("write");

    let text = collect_logs(&logs, now).expect("section");
    let mut out = text.lines();
    assert_eq!(out.next(), Some("## b.log"));
    let projected: Value = serde_json::from_str(out.next().expect("line")).expect("json");
    assert_eq!(
        projected,
        json!({"ts": "2025-06-07T22:25:00Z", "level": "error", "event": "provider_retry", "attempt": 2, "durationMs": 120})
    );
    assert_eq!(out.next(), None);
    assert!(!text.contains("a.log.old"));

    std::fs::remove_file(logs.join("b.log")).expect("rm");
    assert_eq!(collect_logs(&logs, now), None);
}

// ---------------------------------------------------------------------------------------------
// 7

#[test]
fn t07_client_identity_is_validated_and_listed_first() {
    let identity = json!({
        "deviceId": "dev_01:abc@x",
        "email": "pat@example.com",
        "accountId": "acct/1",
        "deviceName": "Pat's\tlaptop",
        "displayName": format!("Pat {SECRET}"),
    });
    let parsed = ClientIdentity::from_json(Some(&identity), &|text: &str| Some(redact(text)));
    assert_eq!(parsed.device_id.as_deref(), Some("dev_01:abc@x"));
    assert_eq!(parsed.email.as_deref(), Some("pat@example.com"));
    assert_eq!(parsed.account_id, None);
    assert_eq!(parsed.device_name.as_deref(), Some("Pat's laptop"));
    assert_eq!(parsed.display_name, None);

    let long = ClientIdentity::from_json(
        Some(&json!({"deviceName": "n".repeat(200)})),
        &|text: &str| Some(redact(text)),
    );
    assert_eq!(long.device_name, None);

    let fx = Fixture::new();
    let draft = fx
        .reports()
        .prepare_json(&json!({"sessionId": null, "identity": identity}));
    let text = section_text(&draft, "identity").expect("identity");
    let device_id = text.find("dev_01:abc@x").expect("device id line");
    let host = text.find("devbox").expect("host line");
    assert!(device_id < host);
    assert!(text.contains("Pat's laptop"));
    assert!(!text.contains("acct/1"));
    assert!(!text.contains(MASK) && !text.contains(SECRET));
    let identity_section = draft["sections"]
        .as_array()
        .expect("sections")
        .iter()
        .find(|s| s["id"] == "identity")
        .expect("identity");
    assert_eq!(identity_section["redactedCount"], 0);
}

// ---------------------------------------------------------------------------------------------
// 8

#[test]
fn t08_client_sections_and_signed_in() {
    let fx = Fixture::new();
    let draft = fx.reports().prepare_json(&json!({
        "sessionId": null,
        "accessToken": "tok",
        "sections": [
            {"id": "crash", "text": format!("panic {SECRET}")},
            {"id": "logs", "text": "client log"},
            {"id": "session", "text": "fake transcript"},
            {"id": "crash", "text": 7}
        ]
    }));
    assert_eq!(draft["signedIn"], true);
    assert_eq!(
        section_ids(&draft),
        [
            "description",
            "identity",
            "environment",
            "logs",
            "crash",
            "config"
        ]
    );
    assert_eq!(
        section_text(&draft, "crash"),
        Some(format!("panic {MASK}").as_str())
    );
    assert_eq!(section_text(&draft, "logs"), Some("client log"));

    let unsigned = fx
        .reports()
        .prepare_json(&json!({"sessionId": null, "accessToken": 5}));
    assert_eq!(unsigned["signedIn"], false);
}

#[test]
fn t08_client_section_precedes_host_section_of_the_same_id() {
    let fx = Fixture::new();
    let logs = fx.agent_dir.join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    std::fs::write(
        logs.join("app.log"),
        json!({"ts": "2025-06-07T22:29:00Z", "event": "tick"}).to_string(),
    )
    .expect("write");
    let draft = fx.reports().prepare_json(&json!({
        "sessionId": null,
        "sections": [{"id": "logs", "text": "client first"}]
    }));
    let text = section_text(&draft, "logs").expect("logs");
    assert!(text.starts_with("client first\n## app.log\n"), "{text}");
}

// ---------------------------------------------------------------------------------------------
// 9

#[test]
fn t09_submit_success_posts_records_and_forgets_the_draft() {
    let fx = Fixture::new();
    fx.set_env("OMO_SERVICE_API_URL", "https://intake.test/");
    let reports = fx.reports();
    let draft = reports.prepare_json(&json!({"sessionId": null}));
    let draft_id = draft["draftId"].as_str().expect("id").to_owned();
    fx.intake
        .respond(201, r#"{"id":"rep_1","deleteToken":"del_1"}"#);
    let description = format!("\n  Crash on {SECRET} save  \nmore details");
    let result = block_on(reports.submit_json(&json!({
        "draftId": draft_id,
        "kind": "bug",
        "description": description,
        "accessToken": "tok-1"
    })));
    assert_eq!(result, json!({"kind": "sent", "reportId": "rep_1"}));

    let requests = fx.intake.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.url, "https://intake.test/v1/reports");
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(
        request.header("idempotency-key"),
        Some(format!("feedback-{draft_id}").as_str())
    );
    assert_eq!(request.header("authorization"), Some("Bearer tok-1"));
    let body: Value = serde_json::from_str(&request.body).expect("body");
    assert_eq!(body["kind"], "bug");
    assert_eq!(body["redactorVersion"], "1");
    assert_eq!(body["title"], format!("Crash on {MASK} save"));
    assert_eq!(body["client"]["omo"], "1.2.3");
    assert_eq!(body["client"]["senpi"], "0.9.0");
    assert_eq!(body["client"]["app"], "cli");
    assert!(body.get("installHash").is_none());
    let sections = body["sections"].as_array().expect("sections");
    assert_eq!(sections[0]["id"], "description");
    assert_eq!(sections[0]["text"], description.replace(SECRET, MASK));
    for (sent, drafted) in sections
        .iter()
        .zip(draft["sections"].as_array().expect("draft"))
        .skip(1)
    {
        assert_eq!(sent["id"], drafted["id"]);
        assert_eq!(sent["text"], drafted["text"]);
    }

    let ledger: Value = serde_json::from_str(
        &std::fs::read_to_string(fx.agent_dir.join("omo-senpi/feedback/sent.json"))
            .expect("ledger"),
    )
    .expect("json");
    let ledger = ledger.as_array().expect("array");
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0]["id"], "rep_1");
    assert_eq!(ledger[0]["deleteToken"], "del_1");
    assert!(ledger[0]["createdAt"].is_string());

    assert!(!fx.store.has_draft(&draft_id));
    let again = block_on(reports.submit_json(&json!({"draftId": draft_id, "description": "x"})));
    assert_eq!(again, json!({"kind": "refused", "reason": "unavailable"}));
    assert_eq!(fx.intake.requests().len(), 1);
}

#[test]
fn t09_sent_ledger_replaces_same_id_and_recovers_from_bad_content() {
    let fx = Fixture::new();
    let dir = fx.agent_dir.join("omo-senpi/feedback");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("sent.json"), "{not an array").expect("write");
    let reports = fx.reports();
    for token in ["del_a", "del_b"] {
        let draft = reports.prepare_json(&json!({"sessionId": null}));
        fx.intake.respond(
            200,
            &json!({"id": "rep_same", "deleteToken": token}).to_string(),
        );
        let result = block_on(
            reports.submit_json(&json!({"draftId": draft["draftId"], "description": "d"})),
        );
        assert_eq!(result["kind"], "sent");
    }
    let ledger: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("sent.json")).expect("read"))
            .expect("json");
    assert_eq!(ledger.as_array().expect("array").len(), 1);
    assert_eq!(ledger[0]["deleteToken"], "del_b");
}

// ---------------------------------------------------------------------------------------------
// 10

#[test]
fn t10_caller_idempotency_key_and_conflict() {
    let fx = Fixture::new();
    let reports = fx.reports();
    let conflict = r#"{"error":{"code":"conflict"}}"#;

    let draft = reports.prepare_json(&json!({"sessionId": null}));
    fx.intake.respond(409, conflict);
    let result = block_on(reports.submit_json(&json!({
        "draftId": draft["draftId"],
        "description": "d",
        "idempotencyKey": "feedback-client_key-1"
    })));
    assert_eq!(result, json!({"kind": "already-sent"}));
    assert_eq!(
        fx.intake.requests()[0].header("idempotency-key"),
        Some("feedback-client_key-1")
    );
    assert!(!fx.store.has_draft(draft["draftId"].as_str().expect("id")));

    let draft = reports.prepare_json(&json!({"sessionId": null}));
    let draft_id = draft["draftId"].as_str().expect("id");
    fx.intake.respond(409, conflict);
    let result = block_on(reports.submit_json(&json!({
        "draftId": draft_id,
        "description": "d",
        "idempotencyKey": "bad key"
    })));
    assert_eq!(result, json!({"kind": "refused", "reason": "unavailable"}));
    assert_eq!(
        fx.intake.requests()[1].header("idempotency-key"),
        Some(format!("feedback-{draft_id}").as_str())
    );
    assert!(fx.intake.requests()[1].header("authorization").is_none());
}

// ---------------------------------------------------------------------------------------------
// 11

#[test]
fn t11_refusals_map_and_rate_limit_keeps_the_draft() {
    let fx = Fixture::new();
    let reports = fx.reports();
    let cases = [
        (
            400,
            r#"{"error":{"code":"secret_detected"}}"#,
            "secret-detected",
            false,
        ),
        (413, "", "too-large", false),
        (
            429,
            r#"{"error":{"code":"rate_limited"}}"#,
            "rate-limited",
            true,
        ),
        (500, "<html>oops</html>", "unavailable", true),
    ];
    for (status, body, reason, kept) in cases {
        let draft = reports.prepare_json(&json!({"sessionId": null}));
        let draft_id = draft["draftId"].as_str().expect("id").to_owned();
        fx.intake.respond(status, body);
        let result =
            block_on(reports.submit_json(&json!({"draftId": draft_id, "description": "d"})));
        assert_eq!(
            result,
            json!({"kind": "refused", "reason": reason}),
            "status {status}"
        );
        assert_eq!(fx.store.has_draft(&draft_id), kept, "status {status}");
        if status == 429 {
            fx.intake.respond(201, r#"{"id":"rep_retry"}"#);
            let retry =
                block_on(reports.submit_json(&json!({"draftId": draft_id, "description": "d"})));
            assert_eq!(retry, json!({"kind": "sent", "reportId": "rep_retry"}));
        }
    }

    let draft = reports.prepare_json(&json!({"sessionId": null}));
    let result =
        block_on(reports.submit_json(&json!({"draftId": draft["draftId"], "description": "d"})));
    assert_eq!(
        result,
        json!({"kind": "refused", "reason": "unavailable"}),
        "network error"
    );

    let draft = reports.prepare_json(&json!({"sessionId": null}));
    fx.intake.respond(202, r#"{"ok":true}"#);
    let result =
        block_on(reports.submit_json(&json!({"draftId": draft["draftId"], "description": "d"})));
    assert_eq!(
        result,
        json!({"kind": "refused", "reason": "unavailable"}),
        "2xx without id"
    );
}

// ---------------------------------------------------------------------------------------------
// 12

#[test]
fn t12_store_caps_at_eight_and_expires_after_thirty_minutes() {
    let fx = Fixture::new();
    let reports = fx.reports();
    let ids: Vec<String> = (0..10)
        .map(|_| {
            reports.prepare_json(&json!({"sessionId": null}))["draftId"]
                .as_str()
                .expect("id")
                .to_owned()
        })
        .collect();
    assert_eq!(fx.store.draft_count(), 8);
    assert!(!fx.store.has_draft(&ids[0]));
    assert!(!fx.store.has_draft(&ids[1]));
    assert!(fx.store.has_draft(&ids[9]));

    fx.advance(SignedDuration::from_secs(31 * 60));
    fx.intake.respond(201, r#"{"id":"never"}"#);
    let result = block_on(reports.submit_json(&json!({"draftId": ids[9], "description": "d"})));
    assert_eq!(result, json!({"kind": "refused", "reason": "unavailable"}));
    assert!(fx.intake.requests().is_empty());

    reports.prepare_json(&json!({"sessionId": null}));
    assert_eq!(
        fx.store.draft_count(),
        1,
        "expired drafts are dropped on prepare"
    );
}

// ---------------------------------------------------------------------------------------------
// 13

#[test]
fn t13_uploads_off_saves_a_private_local_report() {
    let fx = Fixture::new();
    fx.set_env("OMO_DISABLE_FEEDBACK", "1");
    let reports = fx.reports();
    let draft = reports.prepare_json(&json!({"sessionId": null}));
    assert_eq!(draft["destination"], "local");
    let draft_id = draft["draftId"].as_str().expect("id").to_owned();
    let result = block_on(
        reports.submit_json(&json!({"draftId": draft_id, "kind": "feedback", "description": "hi"})),
    );
    let expected = fx
        .agent_dir
        .join("omo-senpi")
        .join("feedback")
        .join(format!("report-{draft_id}.json"));
    assert_eq!(
        result,
        json!({"kind": "saved", "path": expected.to_string_lossy()})
    );
    assert!(fx.intake.requests().is_empty());
    let body: Value =
        serde_json::from_str(&std::fs::read_to_string(&expected).expect("read")).expect("json");
    assert_eq!(body["kind"], "feedback");
    assert_eq!(body["sections"][0]["text"], "hi");
    assert!(!fx.store.has_draft(&draft_id));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file_mode = std::fs::metadata(&expected)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        let dir_mode = std::fs::metadata(expected.parent().expect("dir"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
    }

    let fx = Fixture::new();
    fx.set_env("OMO_DISABLE_FEEDBACK", " OFF ");
    let draft = fx.reports().prepare_json(&json!({"sessionId": null}));
    assert_eq!(draft["destination"], "intake");
    assert_eq!(Destination::Intake.as_str(), "intake");
}

// ---------------------------------------------------------------------------------------------
// 14

fn sample_draft() -> maho_feedback::Draft {
    let fx = Fixture::new();
    draft_of(fx.reports().prepare(PrepareRequest::default()))
}

#[test]
fn t14_view_then_send() {
    let draft = sample_draft();
    let draft_id = draft.draft_id.clone();
    let ui = FakeUi::default();
    ui.inputs
        .lock()
        .expect("lock")
        .push_back(Some("  it froze  ".into()));
    ui.selections
        .lock()
        .expect("lock")
        .extend([Some(VIEW.to_owned()), Some(SEND.to_owned())]);
    let backend = FakeBackend {
        prepare: Ok(PrepareOutcome::Draft(Box::new(draft))),
        submits: Mutex::new(Vec::new()),
        result: SubmitResult::Sent {
            report_id: "rep_42".into(),
        },
    };
    block_on(flow::run(ReportKind::Bug, "   ", &ui, &backend));
    assert_eq!(*ui.asked_input.lock().expect("lock"), 1);
    let titles = ui.titles.lock().expect("lock").clone();
    assert_eq!(titles.len(), 2);
    assert!(titles[0].contains("90"));
    let notices = ui.notices();
    assert_eq!(notices.len(), 2);
    assert_eq!(notices[0].1, NotificationType::Info);
    let headings: Vec<&str> = notices[0]
        .0
        .lines()
        .filter(|line| line.starts_with("## "))
        .collect();
    assert_eq!(
        headings.len(),
        3,
        "identity, environment, config: {headings:?}"
    );
    assert!(!notices[0].0.contains("## Description"));
    assert_eq!(
        *backend.submits.lock().expect("lock"),
        [json!({"draftId": draft_id, "kind": "bug", "description": "it froze"})]
    );
    assert_eq!(notices[1].1, NotificationType::Info);
    assert!(notices[1].0.contains("rep_42"));
}

#[test]
fn t14_cancel_and_prepare_failure() {
    for selection in [Some(CANCEL.to_owned()), None] {
        let ui = FakeUi::default();
        ui.selections.lock().expect("lock").push_back(selection);
        let backend = FakeBackend {
            prepare: Ok(PrepareOutcome::Draft(Box::new(sample_draft()))),
            submits: Mutex::new(Vec::new()),
            result: SubmitResult::AlreadySent,
        };
        block_on(flow::run(ReportKind::Feedback, "nice tool", &ui, &backend));
        assert_eq!(*ui.asked_input.lock().expect("lock"), 0);
        assert!(backend.submits.lock().expect("lock").is_empty());
        assert!(ui.notices().is_empty());
    }

    for answer in [None, Some("   ".to_owned())] {
        let ui = FakeUi::default();
        ui.inputs.lock().expect("lock").push_back(answer);
        let backend = FakeBackend {
            prepare: Ok(PrepareOutcome::Draft(Box::new(sample_draft()))),
            submits: Mutex::new(Vec::new()),
            result: SubmitResult::AlreadySent,
        };
        block_on(flow::run(ReportKind::Bug, "", &ui, &backend));
        assert!(ui.titles.lock().expect("lock").is_empty());
        assert!(backend.submits.lock().expect("lock").is_empty());
        assert!(ui.notices().is_empty());
    }

    let ui = FakeUi::default();
    let backend = FakeBackend {
        prepare: Err("disk full".into()),
        submits: Mutex::new(Vec::new()),
        result: SubmitResult::AlreadySent,
    };
    block_on(flow::run(ReportKind::Bug, "x", &ui, &backend));
    let notices = ui.notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].1, NotificationType::Error);
    assert!(backend.submits.lock().expect("lock").is_empty());
}

#[test]
fn t14_refusals_notify_an_error_with_a_hint() {
    for reason in [
        Refusal::SecretDetected,
        Refusal::TooLarge,
        Refusal::RateLimited,
        Refusal::Unavailable,
    ] {
        let (message, level) = flow::result_notice(&SubmitResult::Refused(reason));
        assert_eq!(level, NotificationType::Error);
        assert!(!message.is_empty());
    }
}

// ---------------------------------------------------------------------------------------------
// 15

#[test]
fn t15_rpc_session_choice_follows_live_sessions() {
    let fx = Fixture::new();
    let reports = fx.reports();
    let say = |text: &str| {
        vec![message_entry(
            "2025-06-07T22:15:09Z",
            json!({"role": "user", "content": text}),
        )]
    };
    fx.store.session_started(live("first", say("from first")));
    fx.store.session_started(live("second", say("from second")));

    let latest = reports.prepare_json(&json!({}));
    let session = section_text(&latest, "session").expect("session");
    assert!(session.contains("second") && session.contains("from second"));

    let explicit = reports.prepare_json(&json!({"sessionId": "first"}));
    assert!(
        section_text(&explicit, "session")
            .expect("session")
            .contains("from first")
    );

    let unknown = reports.prepare_json(&json!({"sessionId": "nope"}));
    assert_eq!(section_text(&unknown, "session"), None);

    let none = reports.prepare_json(&json!({"sessionId": null}));
    assert_eq!(section_text(&none, "session"), None);
    assert!(!section_ids(&none).contains(&"session".to_owned()));

    fx.store.session_ended("second");
    let after = reports.prepare_json(&json!({}));
    assert!(
        section_text(&after, "session")
            .expect("session")
            .contains("from first")
    );

    fx.store.session_ended("first");
    let empty = reports.prepare_json(&json!({}));
    assert_eq!(section_text(&empty, "session"), None);
}
