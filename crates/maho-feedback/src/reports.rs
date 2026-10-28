//! The draft store, prepare and submit (spec 4.1, 4.4, 5), over an injectable [`Host`] so tests
//! can supply the redactor, transport, clock, environment and directories.

use crate::client::{ClientIdentity, client_sections};
use crate::collect::{self, EnvironmentFacts, SessionHeader};
use crate::model::{
    ClientInfo, Destination, Draft, PrepareOutcome, Refusal, ReportKind, Section, SectionId,
    SubmitResult,
};
use jiff::{SignedDuration, Timestamp};
use maho_ext_api::SessionEntry;
use regex::Regex;
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Uploads are off when this is set to anything but empty, `0`, `false`, `no`, `off`.
pub const DISABLE_ENV: &str = "OMO_DISABLE_FEEDBACK";
/// Intake origin override.
pub const SERVICE_URL_ENV: &str = "OMO_SERVICE_API_URL";
/// `desktop` marks the client app as the desktop front end.
pub const SURFACE_ENV: &str = "OMO_NATIVE_SURFACE";
/// Default intake origin.
pub const DEFAULT_SERVICE_URL: &str = "https://api.omo.dev";
/// Report intake path under the origin.
pub const REPORTS_PATH: &str = "/v1/reports";
/// Version of the redaction applied to a report body.
pub const REDACTOR_VERSION: &str = "1";
/// State directory under the agent dir (existing on-disk location).
pub const STATE_DIR_NAME: &str = "omo-senpi";
/// Directory under the state dir holding local reports and the sent ledger.
pub const FEEDBACK_DIR_NAME: &str = "feedback";
/// Ledger of sent reports with their delete tokens.
pub const SENT_LEDGER: &str = "sent.json";
/// How long a draft stays submittable.
pub const DRAFT_TTL: SignedDuration = SignedDuration::from_secs(30 * 60);
/// A prepare evicts the oldest drafts while this many or more are stored.
pub const MAX_DRAFTS: usize = 8;
/// Longest report title, in characters.
pub const TITLE_MAX_CHARS: usize = 120;

/// Text after the upload redactor and how many replacements it made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub count: usize,
}

/// The host upload redactor.
pub type Redactor = Arc<dyn Fn(&str) -> Redacted + Send + Sync>;

/// One intake request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    /// Lower-case header names.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// One intake response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub type TransportFuture = Pin<Box<dyn Future<Output = Result<HttpResponse, String>> + Send>>;
/// POSTs a request; `Err` is a network failure.
pub type Transport = Arc<dyn Fn(HttpRequest) -> TransportFuture + Send + Sync>;
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;
/// The merged user configuration as loaded for a working directory.
pub type ConfigLoader = Arc<dyn Fn(&Path) -> Value + Send + Sync>;

/// Everything the feedback unit takes from its surroundings.
#[derive(Clone)]
pub struct Host {
    /// `None` when this build has no upload redactor; feedback is then unavailable.
    pub redactor: Option<Redactor>,
    pub transport: Transport,
    pub env: EnvLookup,
    pub clock: Clock,
    pub config: ConfigLoader,
    pub agent_dir: PathBuf,
    /// Working directory used for the config when no session is attached.
    pub default_cwd: PathBuf,
    pub product_version: String,
    pub host_version: String,
    pub host_name: String,
    pub os: String,
    pub arch: String,
    pub kernel: String,
}

impl Host {
    fn env(&self, key: &str) -> Option<String> {
        (self.env)(key)
    }

    /// Whether uploads are switched off by [`DISABLE_ENV`].
    pub fn uploads_disabled(&self) -> bool {
        self.env(DISABLE_ENV).is_some_and(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            )
        })
    }

    /// `<origin>/v1/reports` with exactly one slash between.
    pub fn reports_url(&self) -> String {
        let origin = self
            .env(SERVICE_URL_ENV)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_SERVICE_URL.into());
        format!("{}{REPORTS_PATH}", origin.trim_end_matches('/'))
    }

    /// `<agent dir>/omo-senpi`.
    pub fn state_dir(&self) -> PathBuf {
        self.agent_dir.join(STATE_DIR_NAME)
    }

    pub fn feedback_dir(&self) -> PathBuf {
        self.state_dir().join(FEEDBACK_DIR_NAME)
    }

    fn app(&self) -> &'static str {
        if self.env(SURFACE_ENV).as_deref().map(str::trim) == Some("desktop") {
            "desktop"
        } else {
            "cli"
        }
    }

    fn lookup(&self) -> impl Fn(&str) -> Option<String> + '_ {
        move |key: &str| self.env(key)
    }
}

/// A live session a draft can describe.
#[derive(Clone)]
pub struct LiveSession {
    pub session_id: String,
    pub cwd: PathBuf,
    /// `provider/id`.
    pub model: Option<String>,
    pub entries: Arc<dyn Fn() -> Vec<SessionEntry> + Send + Sync>,
}

impl std::fmt::Debug for LiveSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveSession")
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct StoreState {
    /// Oldest first.
    drafts: Vec<Draft>,
    /// In start order; the last one is the latest.
    sessions: Vec<LiveSession>,
}

/// Process-wide drafts and live sessions.
#[derive(Default)]
pub struct Store {
    state: Mutex<StoreState>,
}

impl Store {
    fn lock(&self) -> std::sync::MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records a started session as the latest one.
    pub fn session_started(&self, session: LiveSession) {
        let mut state = self.lock();
        state
            .sessions
            .retain(|known| known.session_id != session.session_id);
        state.sessions.push(session);
    }

    /// Forgets a shut-down session; the latest becomes the most recently added remaining one.
    pub fn session_ended(&self, session_id: &str) {
        self.lock()
            .sessions
            .retain(|known| known.session_id != session_id);
    }

    /// Updates the model of a live session.
    pub fn set_model(&self, session_id: &str, model: Option<String>) {
        if let Some(session) = self
            .lock()
            .sessions
            .iter_mut()
            .find(|known| known.session_id == session_id)
        {
            session.model = model;
        }
    }

    pub fn latest_session(&self) -> Option<LiveSession> {
        self.lock().sessions.last().cloned()
    }

    pub fn session(&self, session_id: &str) -> Option<LiveSession> {
        self.lock()
            .sessions
            .iter()
            .find(|known| known.session_id == session_id)
            .cloned()
    }

    pub fn draft_count(&self) -> usize {
        self.lock().drafts.len()
    }

    pub fn has_draft(&self, draft_id: &str) -> bool {
        self.lock()
            .drafts
            .iter()
            .any(|draft| draft.draft_id == draft_id)
    }

    fn insert(&self, draft: Draft, now: Timestamp) {
        let mut state = self.lock();
        state
            .drafts
            .retain(|stored| now.duration_since(stored.created_at) <= DRAFT_TTL);
        while state.drafts.len() >= MAX_DRAFTS {
            state.drafts.remove(0);
        }
        state.drafts.push(draft);
    }

    /// The draft when it exists and has not expired; an expired one is dropped.
    fn get(&self, draft_id: &str, now: Timestamp) -> Option<Draft> {
        let mut state = self.lock();
        let index = state
            .drafts
            .iter()
            .position(|draft| draft.draft_id == draft_id)?;
        if now.duration_since(state.drafts[index].created_at) > DRAFT_TTL {
            state.drafts.remove(index);
            return None;
        }
        Some(state.drafts[index].clone())
    }

    fn remove(&self, draft_id: &str) {
        self.lock()
            .drafts
            .retain(|draft| draft.draft_id != draft_id);
    }
}

/// The store shared by every session of this process.
pub fn shared_store() -> Arc<Store> {
    static STORE: OnceLock<Arc<Store>> = OnceLock::new();
    Arc::clone(STORE.get_or_init(|| Arc::new(Store::default())))
}

/// Input of a prepare.
#[derive(Clone, Debug, Default)]
pub struct PrepareRequest {
    pub session: Option<LiveSession>,
    pub access_token: Option<String>,
    /// Client `logs` / `crash` additions.
    pub sections: Vec<(SectionId, String)>,
    /// Raw client `identity` object.
    pub identity: Option<Value>,
}

/// Input of a submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitRequest {
    pub draft_id: String,
    pub kind: ReportKind,
    pub description: String,
    pub access_token: Option<String>,
    pub idempotency_key: Option<String>,
}

/// Orders collected sections, merges client text before host text, and runs the redactor over
/// every section except `identity`. A `description` is always present and empty.
pub fn build_sections(
    host_sections: Vec<(SectionId, String)>,
    client: Vec<(SectionId, String)>,
    redact: &dyn Fn(&str) -> Redacted,
) -> Vec<Section> {
    SectionId::ORDER
        .into_iter()
        .filter_map(|id| {
            if id == SectionId::Description {
                return Some(Section {
                    id,
                    text: String::new(),
                    redacted_count: 0,
                });
            }
            let client_text: Vec<&str> = client
                .iter()
                .filter(|(cid, _)| *cid == id)
                .map(|(_, text)| text.as_str())
                .collect();
            let host_text: Vec<&str> = host_sections
                .iter()
                .filter(|(hid, _)| *hid == id)
                .map(|(_, text)| text.as_str())
                .collect();
            if client_text.is_empty() && host_text.is_empty() {
                return None;
            }
            let raw = client_text
                .into_iter()
                .chain(host_text)
                .collect::<Vec<_>>()
                .join("\n");
            Some(if id.redacted() {
                let out = redact(&raw);
                Section {
                    id,
                    text: out.text,
                    redacted_count: out.count,
                }
            } else {
                Section {
                    id,
                    text: raw,
                    redacted_count: 0,
                }
            })
        })
        .collect()
}

/// `^feedback-[A-Za-z0-9_-]{4,96}$`.
pub fn valid_idempotency_key(key: &str) -> bool {
    static CELL: OnceLock<Option<Regex>> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^feedback-[A-Za-z0-9_-]{4,96}$").ok())
        .as_ref()
        .is_some_and(|re| re.is_match(key))
}

/// First non-blank line, trimmed, at most [`TITLE_MAX_CHARS`] characters.
pub fn report_title(description: &str) -> String {
    description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .chars()
        .take(TITLE_MAX_CHARS)
        .collect()
}

/// Maps an intake response (spec 5.2 step 4). `caller_key` is whether the caller's own valid
/// idempotency key was sent.
pub fn map_response(response: &HttpResponse, caller_key: bool) -> SubmitResult {
    let body: Option<Value> = serde_json::from_str(&response.body).ok();
    if (200..300).contains(&response.status) {
        return match body
            .as_ref()
            .and_then(|body| body.get("id"))
            .and_then(Value::as_str)
        {
            Some(id) => SubmitResult::Sent {
                report_id: id.to_owned(),
            },
            None => SubmitResult::Refused(Refusal::Unavailable),
        };
    }
    let code = body
        .as_ref()
        .and_then(|body| body.pointer("/error/code"))
        .and_then(Value::as_str);
    if response.status == 409 && code == Some("conflict") && caller_key {
        return SubmitResult::AlreadySent;
    }
    SubmitResult::Refused(if code == Some("secret_detected") {
        Refusal::SecretDetected
    } else if response.status == 413 {
        Refusal::TooLarge
    } else if response.status == 429 {
        Refusal::RateLimited
    } else {
        Refusal::Unavailable
    })
}

/// Reason given when no upload redactor exists.
pub const NO_REDACTOR_REASON: &str =
    "this build has no upload redactor, so reports cannot be cleaned of secrets before sending";

/// Prepare and submit over one [`Host`] and [`Store`].
#[derive(Clone)]
pub struct Reports {
    pub host: Arc<Host>,
    pub store: Arc<Store>,
}

impl Reports {
    pub fn new(host: Host, store: Arc<Store>) -> Self {
        Self {
            host: Arc::new(host),
            store,
        }
    }

    fn redact(&self, text: &str) -> Option<Redacted> {
        self.host.redactor.as_ref().map(|redactor| redactor(text))
    }

    fn environment(&self, now: Timestamp) -> EnvironmentFacts {
        let host = &self.host;
        let env = host.lookup();
        EnvironmentFacts {
            product_version: host.product_version.clone(),
            host_version: host.host_version.clone(),
            os: host.os.clone(),
            kernel: host.kernel.clone(),
            arch: host.arch.clone(),
            locale: collect::locale(&env),
            channel: collect::channel(&host.product_version).into(),
            report_time: collect::rfc3339_millis(now),
            time_zone: collect::time_zone(&env, now),
        }
    }

    /// Builds and stores a draft (spec 4, 4.4).
    pub fn prepare(&self, request: PrepareRequest) -> PrepareOutcome {
        let Some(redactor) = self.host.redactor.clone() else {
            return PrepareOutcome::Unavailable {
                reason: NO_REDACTOR_REASON.into(),
            };
        };
        let host = &self.host;
        let now = (host.clock)();
        let redact = |text: &str| redactor(text);
        let identity = ClientIdentity::from_json(request.identity.as_ref(), &|text: &str| {
            Some(redactor(text))
        });
        let mut identity_lines = identity.labelled();
        identity_lines.push(("Host name", host.host_name.clone()));
        let identity_text = collect::aligned(
            &identity_lines
                .iter()
                .map(|(label, value)| (*label, value.as_str()))
                .collect::<Vec<_>>(),
        );

        let cwd = request
            .session
            .as_ref()
            .map_or_else(|| host.default_cwd.clone(), |session| session.cwd.clone());
        let mut host_sections = vec![
            (SectionId::Identity, identity_text),
            (SectionId::Environment, self.environment(now).render()),
            (
                SectionId::Config,
                collect::config_text(&(host.config)(cwd.as_path())),
            ),
        ];
        if let Some(logs) = collect::collect_logs(&host.agent_dir.join("logs"), now) {
            host_sections.push((SectionId::Logs, logs));
        }
        if let Some(session) = &request.session {
            let header = SessionHeader {
                session_id: Some(session.session_id.clone()),
                model: session.model.clone(),
                cwd: Some(session.cwd.to_string_lossy().into_owned()),
            };
            host_sections.push((
                SectionId::Session,
                collect::render_session(&header, &(session.entries)()),
            ));
        }

        let env = host.lookup();
        let draft = Draft {
            draft_id: format!("drf_{}", uuid::Uuid::new_v4()),
            sections: build_sections(host_sections, request.sections, &redact),
            destination: if host.uploads_disabled() {
                Destination::Local
            } else {
                Destination::Intake
            },
            signed_in: request.access_token.is_some(),
            client: ClientInfo {
                app: host.app().into(),
                product_version: host.product_version.clone(),
                host_version: host.host_version.clone(),
                os: host.os.clone(),
                arch: host.arch.clone(),
                locale: collect::locale(&|key: &str| (key == "LANG").then(|| env(key)).flatten()),
                channel: collect::channel(&host.product_version).into(),
                device_name: identity.device_name.clone(),
            },
            created_at: now,
        };
        self.store.insert(draft.clone(), now);
        PrepareOutcome::Draft(Box::new(draft))
    }

    /// The report body for `draft` with the redacted `description`.
    pub fn report_body(draft: &Draft, kind: ReportKind, description: &Redacted) -> Value {
        let sections: Vec<Value> = draft
            .sections
            .iter()
            .map(|section| {
                let text = if section.id == SectionId::Description {
                    description.text.as_str()
                } else {
                    section.text.as_str()
                };
                json!({"id": section.id.as_str(), "text": text})
            })
            .collect();
        json!({
            "kind": kind.as_str(),
            "redactorVersion": REDACTOR_VERSION,
            "title": report_title(&description.text),
            "client": draft.client.to_json(),
            "sections": sections,
        })
    }

    /// Submits a stored draft (spec 5.2).
    pub async fn submit(&self, request: SubmitRequest) -> SubmitResult {
        let now = (self.host.clock)();
        let Some(draft) = self.store.get(&request.draft_id, now) else {
            return SubmitResult::Refused(Refusal::Unavailable);
        };
        let Some(description) = self.redact(&request.description) else {
            return SubmitResult::Refused(Refusal::Unavailable);
        };
        let body = Self::report_body(&draft, request.kind, &description);
        let result = match draft.destination {
            Destination::Local => self.save_locally(&draft.draft_id, &body),
            Destination::Intake => self.post(&draft, &request, &body, now).await,
        };
        if result.removes_draft() {
            self.store.remove(&draft.draft_id);
        }
        result
    }

    fn save_locally(&self, draft_id: &str, body: &Value) -> SubmitResult {
        let dir = self.host.feedback_dir();
        let path = dir.join(format!("report-{draft_id}.json"));
        let text = serde_json::to_string_pretty(body).unwrap_or_default();
        match private_dir(&dir).and_then(|()| write_private(&path, &text)) {
            Ok(()) => SubmitResult::Saved {
                path: path.to_string_lossy().into_owned(),
            },
            Err(_) => SubmitResult::Refused(Refusal::Unavailable),
        }
    }

    async fn post(
        &self,
        draft: &Draft,
        request: &SubmitRequest,
        body: &Value,
        now: Timestamp,
    ) -> SubmitResult {
        let caller_key = request
            .idempotency_key
            .as_deref()
            .filter(|key| valid_idempotency_key(key));
        let key = caller_key.map_or_else(|| format!("feedback-{}", draft.draft_id), str::to_owned);
        let mut headers = vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("idempotency-key".to_owned(), key),
        ];
        if let Some(token) = &request.access_token {
            headers.push(("authorization".to_owned(), format!("Bearer {token}")));
        }
        let http = HttpRequest {
            url: self.host.reports_url(),
            headers,
            body: body.to_string(),
        };
        let response = match (self.host.transport)(http).await {
            Ok(response) => response,
            Err(_) => return SubmitResult::Refused(Refusal::Unavailable),
        };
        let result = map_response(&response, caller_key.is_some());
        if let SubmitResult::Sent { report_id } = &result
            && let Some(token) =
                serde_json::from_str::<Value>(&response.body)
                    .ok()
                    .and_then(|body| {
                        body.get("deleteToken")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
        {
            // A ledger failure does not change the result.
            let _ledger = self.record_sent(report_id, &token, now);
        }
        result
    }

    /// Adds `{id, deleteToken, createdAt}` to the sent ledger, replacing an entry with that id.
    fn record_sent(&self, id: &str, token: &str, now: Timestamp) -> std::io::Result<()> {
        let dir = self.host.feedback_dir();
        private_dir(&dir)?;
        let path = dir.join(SENT_LEDGER);
        let mut entries = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        {
            Some(Value::Array(entries)) => entries,
            _ => Vec::new(),
        };
        entries.retain(|entry| entry.get("id").and_then(Value::as_str) != Some(id));
        entries.push(json!({
            "id": id,
            "deleteToken": token,
            "createdAt": collect::rfc3339_millis(now),
        }));
        let text = serde_json::to_string_pretty(&Value::Array(entries)).unwrap_or_default();
        write_private(&path, &text)
    }

    /// The prepare RPC: resolves `sessionId`, then prepares.
    pub fn prepare_json(&self, input: &Value) -> Value {
        let session = match input.get("sessionId") {
            None => self.store.latest_session(),
            Some(Value::String(id)) => self.store.session(id),
            Some(_) => None,
        };
        let request = PrepareRequest {
            session,
            access_token: input
                .get("accessToken")
                .and_then(Value::as_str)
                .map(str::to_owned),
            sections: client_sections(input.get("sections")),
            identity: input.get("identity").cloned(),
        };
        self.prepare(request).to_json()
    }

    /// The submit RPC.
    pub async fn submit_json(&self, input: &Value) -> Value {
        let string = |key: &str| input.get(key).and_then(Value::as_str).map(str::to_owned);
        let request = SubmitRequest {
            draft_id: string("draftId").unwrap_or_default(),
            kind: ReportKind::parse(input.get("kind").and_then(Value::as_str)),
            description: string("description").unwrap_or_default(),
            access_token: string("accessToken"),
            idempotency_key: string("idempotencyKey"),
        };
        self.submit(request).await.to_json()
    }
}

/// Creates `dir` (and parents); on Unix the directory gets mode `0700`.
#[cfg(unix)]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Creates `dir` (and parents) with default permissions.
#[cfg(not(unix))]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Writes `text` to `path` through a temporary sibling; on Unix the file gets mode `0600`.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&tmp).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    match written.and_then(|()| std::fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _cleanup = std::fs::remove_file(&tmp);
            Err(error)
        }
    }
}
