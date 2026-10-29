//! Port of `feedback/submit.ts`: send (or locally save) one report.

use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock};

use chrono::{SecondsFormat, Utc};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::draft::{FeedbackDraft, FeedbackKind, UploadRedactor};

pub const FEEDBACK_REDACTOR_VERSION: &str = "1";
/// Keys this component issues (`feedback-<draftId>`); anything else from a caller is ignored.
pub const FEEDBACK_IDEMPOTENCY_KEY_PATTERN: &str = r"^feedback-[A-Za-z0-9_-]{4,96}$";

static IDEMPOTENCY_KEY: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(FEEDBACK_IDEMPOTENCY_KEY_PATTERN).ok());

pub fn is_feedback_idempotency_key(key: &str) -> bool {
    IDEMPOTENCY_KEY
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(key))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportClient {
    pub app: String,
    pub omo: String,
    pub maho: String,
    pub os: String,
    pub arch: String,
    pub locale: String,
    pub channel: String,
    pub device_name: Option<String>,
}

impl ReportClient {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        map.insert("app".into(), json!(self.app));
        map.insert("omo".into(), json!(self.omo));
        // The intake's wire field keeps the upstream host name.
        map.insert("senpi".into(), json!(self.maho));
        map.insert("os".into(), json!(self.os));
        map.insert("arch".into(), json!(self.arch));
        map.insert("locale".into(), json!(self.locale));
        map.insert("channel".into(), json!(self.channel));
        if let Some(name) = &self.device_name {
            map.insert("deviceName".into(), json!(name));
        }
        Value::Object(map)
    }
}

#[derive(Clone, Debug)]
pub struct TransportRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct TransportResponse {
    pub status: u16,
    pub body: String,
}

pub type TransportFuture = Pin<Box<dyn Future<Output = Result<TransportResponse, String>> + Send>>;
/// POST transport; an `Err` is a network failure.
pub type ReportTransport = Arc<dyn Fn(TransportRequest) -> TransportFuture + Send + Sync>;

/// The production transport: one reqwest POST.
pub fn default_transport() -> ReportTransport {
    Arc::new(|request: TransportRequest| {
        Box::pin(async move {
            let client = reqwest::Client::new();
            let mut builder = client.post(&request.url).body(request.body);
            for (name, value) in &request.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            let response = builder.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            Ok(TransportResponse { status, body })
        })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackSubmitRefusal {
    SecretDetected,
    TooLarge,
    RateLimited,
    Unavailable,
}

impl FeedbackSubmitRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SecretDetected => "secret-detected",
            Self::TooLarge => "too-large",
            Self::RateLimited => "rate-limited",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitResult {
    Sent { report_id: String },
    Saved { path: PathBuf },
    AlreadySent,
    Refused(FeedbackSubmitRefusal),
}

impl SubmitResult {
    /// The wire shape the `omo.feedback.submit` RPC returns.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Sent { report_id } => json!({"kind": "sent", "reportId": report_id}),
            Self::Saved { path } => json!({"kind": "saved", "path": path.to_string_lossy()}),
            Self::AlreadySent => json!({"kind": "already-sent"}),
            Self::Refused(reason) => json!({"kind": "refused", "reason": reason.as_str()}),
        }
    }
}

pub struct SubmitOptions {
    pub draft: FeedbackDraft,
    pub kind: FeedbackKind,
    pub description: String,
    pub redact: UploadRedactor,
    pub client: ReportClient,
    pub env: BTreeMap<String, String>,
    pub state_dir: PathBuf,
    pub api_origin: String,
    pub access_token: Option<String>,
    pub transport: ReportTransport,
    pub install_hash: Option<String>,
    pub session_hash: Option<String>,
    /// The key of an earlier send whose answer was lost. Retrying a fresh draft under it lets the
    /// intake keep one report: another body is a 409 conflict, meaning the first send landed.
    pub idempotency_key: Option<String>,
}

pub fn feedback_uploads_disabled(env: &BTreeMap<String, String>) -> bool {
    env.get("OMO_DISABLE_FEEDBACK").is_some_and(|value| {
        !["", "0", "false", "no", "off"].contains(&value.trim().to_lowercase().as_str())
    })
}

fn first_line(text: &str) -> String {
    text.split('\n')
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(120)
        .collect()
}

#[cfg(unix)]
fn private_write(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn private_write(path: &Path, contents: &str) -> std::io::Result<()> {
    fs::write(path, contents)
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)
}

fn feedback_dir(state_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = state_dir.join("feedback");
    create_private_dir(&dir)?;
    Ok(dir)
}

fn remember_delete_token(state_dir: &Path, id: &str, delete_token: &str) -> std::io::Result<()> {
    let path = feedback_dir(state_dir)?.join("sent.json");
    let mut entries: Vec<Value> = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| match value {
            Value::Array(items) => Some(
                items
                    .into_iter()
                    .filter(|item| item.get("id").is_some_and(Value::is_string))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    entries.retain(|entry| entry.get("id").and_then(Value::as_str) != Some(id));
    entries.push(json!({
        "id": id,
        "deleteToken": delete_token,
        "createdAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    }));
    let text = serde_json::to_string_pretty(&entries).map_err(std::io::Error::other)?;
    private_write(&path, &text)
}

fn refusal_for(status: u16, code: Option<&str>) -> SubmitResult {
    SubmitResult::Refused(if code == Some("secret_detected") {
        FeedbackSubmitRefusal::SecretDetected
    } else if status == 413 {
        FeedbackSubmitRefusal::TooLarge
    } else if status == 429 {
        FeedbackSubmitRefusal::RateLimited
    } else {
        FeedbackSubmitRefusal::Unavailable
    })
}

/// Sends the draft as one report: the description is redacted here (the only text written after the
/// draft was built) and every other section goes exactly as the draft shows it. With
/// OMO_DISABLE_FEEDBACK set the same body is written to a private file and nothing is sent.
pub async fn submit_report(options: SubmitOptions) -> SubmitResult {
    let description = (options.redact)(&options.description).text;
    let sections: Vec<Value> = options
        .draft
        .sections
        .iter()
        .map(|section| {
            let text = if section.id == crate::draft::FeedbackSectionId::Description {
                &description
            } else {
                &section.text
            };
            json!({"id": section.id.as_str(), "text": text})
        })
        .collect();
    let mut body = Map::new();
    body.insert("kind".into(), json!(options.kind.as_str()));
    body.insert("redactorVersion".into(), json!(FEEDBACK_REDACTOR_VERSION));
    body.insert("title".into(), json!(first_line(&description)));
    body.insert("client".into(), options.client.to_json());
    if let Some(hash) = &options.install_hash {
        body.insert("installHash".into(), json!(hash));
    }
    if let Some(hash) = &options.session_hash {
        body.insert("sessionHash".into(), json!(hash));
    }
    body.insert("sections".into(), Value::Array(sections));
    let Ok(json_body) = serde_json::to_string(&Value::Object(body)) else {
        return SubmitResult::Refused(FeedbackSubmitRefusal::Unavailable);
    };

    if feedback_uploads_disabled(&options.env) {
        let saved = feedback_dir(&options.state_dir).and_then(|dir| {
            let path = dir.join(format!("report-{}.json", options.draft.draft_id));
            private_write(&path, &json_body).map(|()| path)
        });
        return match saved {
            Ok(path) => SubmitResult::Saved { path },
            Err(_) => SubmitResult::Refused(FeedbackSubmitRefusal::Unavailable),
        };
    }

    let kept_key = options
        .idempotency_key
        .as_deref()
        .filter(|key| is_feedback_idempotency_key(key))
        .map(str::to_string);
    let mut headers = vec![
        ("content-type".to_string(), "application/json".to_string()),
        (
            "idempotency-key".to_string(),
            kept_key
                .clone()
                .unwrap_or_else(|| format!("feedback-{}", options.draft.draft_id)),
        ),
    ];
    if let Some(token) = &options.access_token {
        headers.push(("authorization".to_string(), format!("Bearer {token}")));
    }
    let request = TransportRequest {
        url: format!("{}/v1/reports", options.api_origin.trim_end_matches('/')),
        headers,
        body: json_body,
    };
    let Ok(response) = (options.transport)(request).await else {
        return SubmitResult::Refused(FeedbackSubmitRefusal::Unavailable);
    };
    let answer: Option<Value> = serde_json::from_str(&response.body).ok();
    if !(200..300).contains(&response.status) {
        let code = answer
            .as_ref()
            .and_then(|answer| answer.get("error"))
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str);
        if kept_key.is_some() && response.status == 409 && code == Some("conflict") {
            return SubmitResult::AlreadySent;
        }
        return refusal_for(response.status, code);
    }
    let Some(report_id) = answer
        .as_ref()
        .and_then(|answer| answer.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return SubmitResult::Refused(FeedbackSubmitRefusal::Unavailable);
    };
    if let Some(delete_token) = answer
        .as_ref()
        .and_then(|answer| answer.get("deleteToken"))
        .and_then(Value::as_str)
    {
        // The report was accepted; losing the local delete token only means the user deletes it by
        // signing in.
        let _ = remember_delete_token(&options.state_dir, &report_id, delete_token);
    }
    SubmitResult::Sent { report_id }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft::{
        BuildDraftOptions, CollectedSection, FeedbackDestination, FeedbackSectionId, Redaction,
        build_draft,
    };
    use std::sync::Mutex;

    fn redactor() -> UploadRedactor {
        Arc::new(|text: &str| Redaction {
            text: text.replace("SECRET", "<redacted>"),
            count: text.matches("SECRET").count(),
        })
    }

    fn draft(redact: &UploadRedactor) -> FeedbackDraft {
        build_draft(BuildDraftOptions {
            draft_id: "drf_abcd".into(),
            collected: &[CollectedSection::new(FeedbackSectionId::Environment, "env")],
            redact,
            destination: FeedbackDestination::Intake,
            signed_in: false,
        })
    }

    fn options(
        state_dir: &Path,
        env: BTreeMap<String, String>,
        key: Option<&str>,
        transport: ReportTransport,
    ) -> SubmitOptions {
        let redact = redactor();
        SubmitOptions {
            draft: draft(&redact),
            kind: FeedbackKind::Bug,
            description: "  \nbroke SECRET\nmore".into(),
            redact,
            client: ReportClient {
                app: "cli".into(),
                omo: "1.0.0".into(),
                maho: "2.0.0".into(),
                os: "linux".into(),
                arch: "x86_64".into(),
                locale: "en-US".into(),
                channel: "latest".into(),
                device_name: None,
            },
            env,
            state_dir: state_dir.to_path_buf(),
            api_origin: "https://api.test/".into(),
            access_token: Some("tok".into()),
            transport,
            install_hash: Some("ih".into()),
            session_hash: None,
            idempotency_key: key.map(str::to_string),
        }
    }

    type Seen = Arc<Mutex<Vec<TransportRequest>>>;

    fn scripted(status: u16, body: &str) -> (ReportTransport, Seen) {
        let seen: Seen = Arc::default();
        let log = Arc::clone(&seen);
        let body = body.to_string();
        let transport: ReportTransport = Arc::new(move |request| {
            log.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request);
            let body = body.clone();
            Box::pin(async move { Ok(TransportResponse { status, body }) })
        });
        (transport, seen)
    }

    #[tokio::test]
    async fn sends_redacted_report_and_remembers_delete_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (transport, seen) = scripted(201, r#"{"id":"rep_1","deleteToken":"del"}"#);
        let result = submit_report(options(dir.path(), BTreeMap::new(), None, transport)).await;
        assert_eq!(
            result,
            SubmitResult::Sent {
                report_id: "rep_1".into()
            }
        );
        let requests = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let request = &requests[0];
        assert_eq!(request.url, "https://api.test/v1/reports");
        assert!(
            request
                .headers
                .contains(&("idempotency-key".into(), "feedback-drf_abcd".into()))
        );
        assert!(
            request
                .headers
                .contains(&("authorization".into(), "Bearer tok".into()))
        );
        let body: Value = serde_json::from_str(&request.body).expect("json body");
        assert_eq!(body["title"], "broke <redacted>");
        assert_eq!(body["redactorVersion"], "1");
        assert_eq!(
            body["sections"][0]["text"],
            "  \nbroke <redacted>\nmore".replace("SECRET", "<redacted>")
        );
        let sent = fs::read_to_string(dir.path().join("feedback/sent.json")).expect("sent.json");
        assert!(sent.contains("\"deleteToken\": \"del\""));
    }

    #[tokio::test]
    async fn kept_idempotency_key_turns_conflict_into_already_sent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (transport, seen) = scripted(409, r#"{"error":{"code":"conflict"}}"#);
        let result = submit_report(options(
            dir.path(),
            BTreeMap::new(),
            Some("feedback-drf_old1"),
            Arc::clone(&transport),
        ))
        .await;
        assert_eq!(result, SubmitResult::AlreadySent);
        {
            let requests = seen
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(
                requests[0]
                    .headers
                    .contains(&("idempotency-key".into(), "feedback-drf_old1".into()))
            );
        }
        let ignored = submit_report(options(
            dir.path(),
            BTreeMap::new(),
            Some("bad key"),
            transport,
        ))
        .await;
        assert_eq!(
            ignored,
            SubmitResult::Refused(FeedbackSubmitRefusal::Unavailable)
        );
    }

    #[tokio::test]
    async fn maps_refusals() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (status, body, expected) in [
            (
                400,
                r#"{"error":{"code":"secret_detected"}}"#,
                FeedbackSubmitRefusal::SecretDetected,
            ),
            (413, "{}", FeedbackSubmitRefusal::TooLarge),
            (429, "{}", FeedbackSubmitRefusal::RateLimited),
            (500, "nope", FeedbackSubmitRefusal::Unavailable),
        ] {
            let (transport, _) = scripted(status, body);
            let result = submit_report(options(dir.path(), BTreeMap::new(), None, transport)).await;
            assert_eq!(result, SubmitResult::Refused(expected));
        }
    }

    #[tokio::test]
    async fn disabled_uploads_save_a_private_file_and_never_send() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (transport, seen) = scripted(201, "{}");
        let env = BTreeMap::from([("OMO_DISABLE_FEEDBACK".to_string(), "1".to_string())]);
        let result = submit_report(options(dir.path(), env, None, transport)).await;
        let SubmitResult::Saved { path } = result else {
            panic!("expected saved")
        };
        assert_eq!(path, dir.path().join("feedback/report-drf_abcd.json"));
        assert!(
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).expect("meta").permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(dir.path().join("feedback"))
                    .expect("meta")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }
}
