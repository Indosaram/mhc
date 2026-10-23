//! Port of `feedback/runtime.ts`: the draft store, the upload and the TUI command flow.
//!
//! Upstream split this file into a lazily imported sidecar; natively it is an ordinary module, so
//! the TUI flow lives here and always loads.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};

use chrono::{DateTime, Utc};
use maho_ext_api::{NotificationType, UiFuture};
use regex::Regex;
use serde_json::Value;

use crate::collect_host::{HostFacts, HostSessionView, collect_host_sections};
use crate::draft::{
    BuildDraftOptions, CollectedSection, FeedbackDestination, FeedbackDraft, FeedbackKind,
    FeedbackSectionId, UploadRedactor, build_draft,
};
use crate::submit::{
    FeedbackSubmitRefusal, ReportClient, ReportTransport, SubmitOptions, SubmitResult,
    feedback_uploads_disabled, submit_report,
};

const DRAFT_TTL_MS: i64 = 30 * 60 * 1000;
pub const MAX_DRAFTS: usize = 8;

/// The live session a report can include. `entries` are raw session entry objects.
pub type FeedbackSession = HostSessionView;

pub struct FeedbackCoreConfig {
    pub env: BTreeMap<String, String>,
    pub now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    pub omo_version: String,
    pub maho_version: String,
    pub channel: String,
    pub state_dir: Arc<dyn Fn() -> std::path::PathBuf + Send + Sync>,
    pub api_origin: Arc<dyn Fn() -> String + Send + Sync>,
    pub transport: ReportTransport,
}

#[derive(Clone)]
struct StoredDraft {
    draft: FeedbackDraft,
    client: ReportClient,
    install_hash: Option<String>,
    session_hash: Option<String>,
    expires_at: i64,
}

pub struct FeedbackCore {
    config: FeedbackCoreConfig,
    /// Insertion-ordered like a JS `Map`: the oldest draft is evicted first.
    drafts: Mutex<Vec<(String, StoredDraft)>>,
}

fn string_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

/// Client-supplied sections: only `logs` and `crash`, only with string text.
fn client_sections(input: &Value) -> Vec<CollectedSection> {
    let Some(raw) = input.get("sections").and_then(Value::as_array) else {
        return Vec::new();
    };
    raw.iter()
        .filter_map(|entry| {
            let id = FeedbackSectionId::parse(entry.get("id")?.as_str()?)?;
            if !matches!(id, FeedbackSectionId::Logs | FeedbackSectionId::Crash) {
                return None;
            }
            Some(CollectedSection::new(id, entry.get("text")?.as_str()?))
        })
        .collect()
}

static ID_VALUE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.:@-]{1,128}$").ok());
static EMAIL_VALUE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^[^\s@]{1,64}@[^\s@]{1,190}$").ok());

#[derive(Clone, Copy)]
enum Shape {
    Id,
    Email,
    Free,
}

const IDENTITY_FIELDS: [(&str, &str, Shape); 7] = [
    ("account", "accountId", Shape::Id),
    ("user", "userId", Shape::Id),
    ("email", "email", Shape::Email),
    ("name", "displayName", Shape::Free),
    ("plan", "plan", Shape::Id),
    ("device", "deviceId", Shape::Id),
    ("device name", "deviceName", Shape::Free),
];

fn matches_shape(pattern: &LazyLock<Option<Regex>>, value: &str) -> bool {
    // A pattern that failed to build rejects every value (fail closed).
    pattern.as_ref().is_some_and(|regex| regex.is_match(value))
}

fn single_line(value: &str) -> Option<String> {
    let mut collapsed = String::new();
    let mut in_control = false;
    for ch in value.chars() {
        if (ch as u32) < 0x20 || ch == '\u{7f}' {
            if !in_control {
                collapsed.push(' ');
            }
            in_control = true;
        } else {
            in_control = false;
            collapsed.push(ch);
        }
    }
    let line = collapsed.trim();
    (!line.is_empty() && line.encode_utf16().count() <= 120).then(|| line.to_string())
}

fn label(label: &str, value: &str) -> String {
    format!("{label:<12}{value}")
}

/// Builds the desktop's identity lines from named fields only. Every value goes through the upload
/// redactor and is dropped when the redactor finds anything in it; ids and the email must also match
/// their shape. The email is checked as its two halves so the redactor's own email rule does not
/// remove the address we are collecting on purpose.
pub fn client_identity_lines(input: &Value, redact: &UploadRedactor) -> Vec<String> {
    let Some(identity) = input.get("identity").filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let clean = |value: &str| redact(value).count == 0;
    IDENTITY_FIELDS
        .iter()
        .filter_map(|(name, key, shape)| {
            let value = identity.get(*key)?.as_str()?;
            match shape {
                Shape::Id => {
                    (matches_shape(&ID_VALUE, value) && clean(value)).then(|| label(name, value))
                }
                Shape::Email => {
                    if !matches_shape(&EMAIL_VALUE, value) {
                        return None;
                    }
                    let at = value.rfind('@')?;
                    (clean(&value[..at]) && clean(&value[at + 1..])).then(|| label(name, value))
                }
                Shape::Free => {
                    let line = single_line(value)?;
                    clean(&line).then(|| label(name, &line))
                }
            }
        })
        .collect()
}

fn merge_sections(
    host: Vec<CollectedSection>,
    client: Vec<CollectedSection>,
    identity_lines: &[String],
) -> Vec<CollectedSection> {
    let mut merged = host;
    for section in client {
        match merged.iter_mut().find(|existing| existing.id == section.id) {
            Some(existing) => {
                let text = format!("{}\n{}", section.text, existing.text);
                *existing = CollectedSection { text, ..section };
            }
            None => merged.push(section),
        }
    }
    if !identity_lines.is_empty() {
        let position = merged
            .iter()
            .position(|section| section.id == FeedbackSectionId::Identity);
        let mut lines = identity_lines.to_vec();
        if let Some(index) = position {
            lines.push(merged[index].text.clone());
        }
        let section = CollectedSection::verbatim(FeedbackSectionId::Identity, lines.join("\n"));
        match position {
            Some(index) => merged[index] = section,
            None => merged.push(section),
        }
    }
    merged
}

impl FeedbackCore {
    pub fn new(config: FeedbackCoreConfig) -> Self {
        Self {
            config,
            drafts: Mutex::new(Vec::new()),
        }
    }

    fn drafts(&self) -> std::sync::MutexGuard<'_, Vec<(String, StoredDraft)>> {
        self.drafts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `session` is `None` for a report without a conversation (also the excluded-session case).
    pub fn prepare(
        &self,
        session: Option<FeedbackSession>,
        input: &Value,
        redact: &UploadRedactor,
    ) -> FeedbackDraft {
        let config = &self.config;
        let now = (config.now)();
        let host = collect_host_sections(&HostFacts {
            env: config.env.clone(),
            omo_version: config.omo_version.clone(),
            maho_version: config.maho_version.clone(),
            channel: config.channel.clone(),
            now,
            session,
        });
        let signed_in = input.get("accessToken").is_some_and(Value::is_string);
        let draft = build_draft(BuildDraftOptions {
            draft_id: format!("drf_{}", uuid::Uuid::new_v4()),
            collected: &merge_sections(
                host.sections,
                client_sections(input),
                &client_identity_lines(input, redact),
            ),
            redact,
            destination: if feedback_uploads_disabled(&config.env) {
                FeedbackDestination::Local
            } else {
                FeedbackDestination::Intake
            },
            signed_in,
        });
        let now_ms = (config.now)().timestamp_millis();
        let mut drafts = self.drafts();
        drafts.retain(|(_, stored)| stored.expires_at >= now_ms);
        while drafts.len() >= MAX_DRAFTS {
            drafts.remove(0);
        }
        let app = if config.env.get("OMO_NATIVE_SURFACE").map(String::as_str) == Some("desktop") {
            "desktop"
        } else {
            "cli"
        };
        drafts.push((
            draft.draft_id.clone(),
            StoredDraft {
                draft: draft.clone(),
                client: ReportClient {
                    app: app.into(),
                    omo: config.omo_version.clone(),
                    maho: config.maho_version.clone(),
                    os: std::env::consts::OS.into(),
                    arch: std::env::consts::ARCH.into(),
                    locale: config
                        .env
                        .get("LANG")
                        .and_then(|lang| lang.split('.').next())
                        .map(|lang| lang.replace('_', "-"))
                        .filter(|lang| !lang.is_empty() && lang != "C" && lang != "POSIX")
                        .unwrap_or_else(|| "unknown".into()),
                    channel: config.channel.clone(),
                    device_name: None,
                },
                install_hash: host.install_hash,
                session_hash: host.session_hash,
                expires_at: now_ms + DRAFT_TTL_MS,
            },
        ));
        draft
    }

    /// An unknown (or expired) draft is `unavailable`. A finished draft is dropped, except when the
    /// intake refused it for a reason a retry can fix (network, rate limit).
    pub async fn submit(&self, input: &Value, redact: &UploadRedactor) -> SubmitResult {
        let stored = string_field(input, "draftId").and_then(|id| {
            self.drafts()
                .iter()
                .find(|(key, _)| key == id)
                .map(|(_, stored)| stored.clone())
        });
        let Some(stored) = stored else {
            return SubmitResult::Refused(FeedbackSubmitRefusal::Unavailable);
        };
        let result = submit_report(SubmitOptions {
            draft: stored.draft.clone(),
            kind: FeedbackKind::from_wire(string_field(input, "kind")),
            description: string_field(input, "description").unwrap_or("").to_string(),
            redact: Arc::clone(redact),
            client: stored.client.clone(),
            env: self.config.env.clone(),
            state_dir: (self.config.state_dir)(),
            api_origin: (self.config.api_origin)(),
            access_token: string_field(input, "accessToken").map(str::to_string),
            transport: Arc::clone(&self.config.transport),
            install_hash: stored.install_hash.clone(),
            session_hash: stored.session_hash.clone(),
            idempotency_key: string_field(input, "idempotencyKey").map(str::to_string),
        })
        .await;
        let keep = matches!(
            result,
            SubmitResult::Refused(
                FeedbackSubmitRefusal::RateLimited | FeedbackSubmitRefusal::Unavailable
            )
        );
        if !keep {
            self.drafts()
                .retain(|(key, _)| *key != stored.draft.draft_id);
        }
        result
    }

    #[cfg(test)]
    fn draft_count(&self) -> usize {
        self.drafts().len()
    }
}

/// The three prompts the TUI flow needs; the extension host's UI implements it in `index`.
pub trait FeedbackCommandUi: Send + Sync {
    fn input<'a>(&'a self, title: &'a str, placeholder: &'a str) -> UiFuture<'a, Option<String>>;
    fn select<'a>(&'a self, title: &'a str, options: &'a [String]) -> UiFuture<'a, Option<String>>;
    fn notify(&self, message: &str, kind: NotificationType);
}

pub const CHOICE_SEND: &str = "Send report";
pub const CHOICE_SEE: &str = "See exactly what is sent";
pub const CHOICE_CANCEL: &str = "Cancel";

/// The TUI flow behind /feedback and /bug: describe, see the notice, optionally see exactly what is
/// sent, then Send.
pub async fn run_feedback_command<P, PF, S, SF>(
    kind: FeedbackKind,
    args: &str,
    ui: &dyn FeedbackCommandUi,
    prepare: P,
    submit: S,
) where
    P: FnOnce() -> PF,
    PF: Future<Output = Result<FeedbackDraft, String>>,
    S: FnOnce(Value) -> SF,
    SF: Future<Output = SubmitResult>,
{
    let prepared = match prepare().await {
        Ok(draft) => draft,
        Err(message) => {
            ui.notify(&message, NotificationType::Error);
            return;
        }
    };
    let description = if args.trim().is_empty() {
        match ui
            .input(
                "What went wrong?",
                "What happened, and what did you expect?",
            )
            .await
        {
            Some(description) => description,
            None => return,
        }
    } else {
        args.trim().to_string()
    };
    if description.trim().is_empty() {
        return;
    }
    let notice = notice_for(&prepared);
    let options = [
        CHOICE_SEND.to_string(),
        CHOICE_SEE.to_string(),
        CHOICE_CANCEL.to_string(),
    ];
    loop {
        match ui.select(&notice, &options).await.as_deref() {
            Some(CHOICE_SEE) => {
                let shown = prepared
                    .sections
                    .iter()
                    .filter(|section| section.id != FeedbackSectionId::Description)
                    .map(|section| format!("## {}\n{}", section.title, section.text))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                ui.notify(&shown, NotificationType::Info);
            }
            Some(CHOICE_SEND) => break,
            _ => return,
        }
    }
    let result = submit(serde_json::json!({
        "draftId": prepared.draft_id,
        "kind": kind.as_str(),
        "description": description,
    }))
    .await;
    let level = if matches!(result, SubmitResult::Refused(_)) {
        NotificationType::Error
    } else {
        NotificationType::Info
    };
    ui.notify(&result_message(&result), level);
}

fn section_phrase(id: FeedbackSectionId) -> Option<&'static str> {
    match id {
        FeedbackSectionId::Description => None,
        FeedbackSectionId::Identity => Some("your account and this device"),
        FeedbackSectionId::Environment => Some("app versions"),
        FeedbackSectionId::Session => Some("this conversation's recent messages"),
        FeedbackSectionId::Logs => Some("recent logs"),
        FeedbackSectionId::Crash => Some("crash reports"),
        FeedbackSectionId::Config => Some("your settings"),
    }
}

pub fn notice_for(draft: &FeedbackDraft) -> String {
    let phrases: Vec<&str> = draft
        .sections
        .iter()
        .filter_map(|section| section_phrase(section.id))
        .collect();
    let included = match phrases.split_last() {
        None => String::new(),
        Some((only, [])) => (*only).to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    };
    match draft.destination {
        FeedbackDestination::Intake => format!(
            "Sending also includes {included}. Passwords, keys and tokens are removed first, and your memory is never sent. Only the OmO team can read it; it is kept {} days.",
            draft.retention_days
        ),
        FeedbackDestination::Local => format!(
            "Uploads are turned off here (OMO_DISABLE_FEEDBACK), so the report, with {included}, is saved as a file you can send yourself."
        ),
    }
}

pub fn result_message(result: &SubmitResult) -> String {
    match result {
        SubmitResult::Sent { report_id } => format!("Thanks. Your report was sent ({report_id})."),
        SubmitResult::Saved { path } => {
            format!("Uploads are off, so the report was saved to {}.", path.display())
        }
        SubmitResult::AlreadySent => {
            "Your earlier report already reached the OmO team, so it was not sent again.".into()
        }
        SubmitResult::Refused(reason) => match reason {
            FeedbackSubmitRefusal::SecretDetected => "The report still looked like it contained a password or key, so nothing was sent. Remove it from the description and try again.",
            FeedbackSubmitRefusal::TooLarge => "The report is too large to send. Try again from a shorter conversation.",
            FeedbackSubmitRefusal::RateLimited => "Too many reports in a short time. Try again in a few minutes.",
            FeedbackSubmitRefusal::Unavailable => "The report could not be sent right now. Check your connection and try again.",
        }
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft::Redaction;
    use crate::submit::TransportResponse;
    use serde_json::json;
    use std::collections::VecDeque;

    fn redactor() -> UploadRedactor {
        Arc::new(|text: &str| Redaction {
            text: text.replace("SECRET", "<redacted>"),
            count: text.matches("SECRET").count(),
        })
    }

    fn core(dir: &std::path::Path, status: u16, body: &str) -> FeedbackCore {
        let state = dir.to_path_buf();
        let body = body.to_string();
        let mut env = BTreeMap::new();
        env.insert("OMO_NATIVE_INSTALL_ID".to_string(), "b".repeat(64));
        env.insert(
            "OMO_NATIVE_SEND_ANONYMOUS_TELEMETRY".to_string(),
            "0".to_string(),
        );
        env.insert("HOME".to_string(), dir.to_string_lossy().into_owned());
        env.insert(
            "MAHO_CODING_AGENT_DIR".to_string(),
            dir.to_string_lossy().into_owned(),
        );
        FeedbackCore::new(FeedbackCoreConfig {
            env,
            now: Arc::new(Utc::now),
            omo_version: "1.0.0".into(),
            maho_version: "2.0.0".into(),
            channel: "latest".into(),
            state_dir: Arc::new(move || state.clone()),
            api_origin: Arc::new(|| "https://api.test".into()),
            transport: Arc::new(move |_| {
                let body = body.clone();
                Box::pin(async move { Ok(TransportResponse { status, body }) })
            }),
        })
    }

    #[test]
    fn identity_lines_enforce_shape_and_redactor_cleanliness() {
        let input = json!({"identity": {
            "accountId": "acct_1", "userId": "bad id", "email": "a@example.com",
            "displayName": "Ada\nLovelace", "plan": "SECRET", "deviceName": "x".repeat(121)
        }});
        let lines = client_identity_lines(&input, &redactor());
        assert_eq!(
            lines,
            [
                "account     acct_1".to_string(),
                "email       a@example.com".to_string(),
                "name        Ada Lovelace".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn prepare_merges_client_sections_and_submit_drops_finished_drafts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let core = core(dir.path(), 201, r#"{"id":"rep_9"}"#);
        let redact = redactor();
        let input = json!({"sections": [
            {"id": "logs", "text": "client log SECRET"},
            {"id": "session", "text": "dropped"},
            {"id": "crash", "text": "boom"}
        ], "accessToken": "t", "identity": {"accountId": "acct_1"}});
        let draft = core.prepare(None, &input, &redact);
        let ids: Vec<&str> = draft
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "description",
                "identity",
                "environment",
                "logs",
                "crash",
                "config"
            ]
        );
        assert!(draft.signed_in);
        let logs = draft
            .sections
            .iter()
            .find(|s| s.id == FeedbackSectionId::Logs)
            .expect("logs");
        assert_eq!(logs.text, "client log <redacted>");
        let identity = draft
            .sections
            .iter()
            .find(|s| s.id == FeedbackSectionId::Identity)
            .expect("identity");
        assert!(identity.text.starts_with("account     acct_1\ndevice    "));
        assert_eq!(core.draft_count(), 1);

        let result = core
            .submit(
                &json!({"draftId": draft.draft_id, "kind": "feedback", "description": "hi"}),
                &redact,
            )
            .await;
        assert_eq!(
            result,
            SubmitResult::Sent {
                report_id: "rep_9".into()
            }
        );
        assert_eq!(core.draft_count(), 0);
        let again = core
            .submit(&json!({"draftId": draft.draft_id}), &redact)
            .await;
        assert_eq!(
            again,
            SubmitResult::Refused(FeedbackSubmitRefusal::Unavailable)
        );
    }

    #[tokio::test]
    async fn retryable_refusals_keep_the_draft_and_the_store_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let core = core(dir.path(), 429, "{}");
        let redact = redactor();
        let first = core.prepare(None, &json!({}), &redact);
        let result = core
            .submit(&json!({"draftId": first.draft_id}), &redact)
            .await;
        assert_eq!(
            result,
            SubmitResult::Refused(FeedbackSubmitRefusal::RateLimited)
        );
        assert_eq!(core.draft_count(), 1);
        for _ in 0..(MAX_DRAFTS + 3) {
            core.prepare(None, &json!({}), &redact);
        }
        assert_eq!(core.draft_count(), MAX_DRAFTS);
    }

    struct ScriptedUi {
        inputs: Mutex<VecDeque<Option<String>>>,
        selects: Mutex<VecDeque<Option<String>>>,
        notices: Mutex<Vec<(String, NotificationType)>>,
    }

    impl ScriptedUi {
        fn new(inputs: &[Option<&str>], selects: &[Option<&str>]) -> Self {
            let own = |items: &[Option<&str>]| {
                items
                    .iter()
                    .map(|item| item.map(str::to_string))
                    .collect::<VecDeque<_>>()
            };
            Self {
                inputs: Mutex::new(own(inputs)),
                selects: Mutex::new(own(selects)),
                notices: Mutex::default(),
            }
        }
    }

    impl FeedbackCommandUi for ScriptedUi {
        fn input<'a>(&'a self, _: &'a str, _: &'a str) -> UiFuture<'a, Option<String>> {
            let next = self
                .inputs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .flatten();
            Box::pin(async move { next })
        }
        fn select<'a>(&'a self, _: &'a str, _: &'a [String]) -> UiFuture<'a, Option<String>> {
            let next = self
                .selects
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .flatten();
            Box::pin(async move { next })
        }
        fn notify(&self, message: &str, kind: NotificationType) {
            self.notices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((message.to_string(), kind));
        }
    }

    fn sample_draft() -> FeedbackDraft {
        let redact = redactor();
        build_draft(BuildDraftOptions {
            draft_id: "drf_1".into(),
            collected: &[CollectedSection::new(FeedbackSectionId::Config, "{}")],
            redact: &redact,
            destination: FeedbackDestination::Intake,
            signed_in: false,
        })
    }

    #[tokio::test]
    async fn command_flow_shows_report_then_sends() {
        let ui = ScriptedUi::new(&[Some("it broke")], &[Some(CHOICE_SEE), Some(CHOICE_SEND)]);
        let submitted: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log = Arc::clone(&submitted);
        run_feedback_command(
            FeedbackKind::Bug,
            "  ",
            &ui,
            || async { Ok(sample_draft()) },
            move |input| async move {
                log.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(input);
                SubmitResult::Sent {
                    report_id: "rep_1".into(),
                }
            },
        )
        .await;
        let sent = submitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            sent[0],
            json!({"draftId": "drf_1", "kind": "bug", "description": "it broke"})
        );
        let notices = ui
            .notices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(notices[0].0, "## Settings\n{}");
        assert_eq!(
            notices[1],
            (
                "Thanks. Your report was sent (rep_1).".to_string(),
                NotificationType::Info
            )
        );
    }

    #[tokio::test]
    async fn command_flow_cancel_and_prepare_failure_send_nothing() {
        let ui = ScriptedUi::new(&[], &[Some(CHOICE_CANCEL)]);
        run_feedback_command(
            FeedbackKind::Feedback,
            "hello",
            &ui,
            || async { Ok(sample_draft()) },
            |_| async { panic!("must not submit") },
        )
        .await;
        assert!(
            ui.notices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        let failing = ScriptedUi::new(&[], &[]);
        run_feedback_command(
            FeedbackKind::Feedback,
            "hello",
            &failing,
            || async { Err("nope".to_string()) },
            |_| async { panic!("must not submit") },
        )
        .await;
        assert_eq!(
            failing
                .notices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)[0],
            ("nope".to_string(), NotificationType::Error)
        );
    }

    #[test]
    fn messages_match_upstream() {
        assert_eq!(
            result_message(&SubmitResult::AlreadySent),
            "Your earlier report already reached the OmO team, so it was not sent again."
        );
        assert_eq!(
            notice_for(&sample_draft()),
            "Sending also includes your settings. Passwords, keys and tokens are removed first, and your memory is never sent. Only the OmO team can read it; it is kept 90 days."
        );
    }
}
