//! The `/feedback` and `/bug` command flow (spec 6) over small dialog and backend interfaces, so
//! the same flow runs against the session UI and against test doubles.

use crate::model::{
    Destination, Draft, PrepareOutcome, Refusal, ReportKind, SectionId, SubmitResult,
};
use crate::reports::{DISABLE_ENV, PrepareRequest, Reports};
use maho_ext_api::{ExtensionUi, ExtensionUiDialogOptions, NotificationType};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;

pub type FlowFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Selection option labels.
pub const SEND: &str = "Send report";
pub const VIEW: &str = "View exact content";
pub const CANCEL: &str = "Cancel";

/// The dialogs the flow needs.
pub trait FlowUi: Send + Sync {
    /// A text input; `None` when cancelled.
    fn input<'a>(&'a self, title: &'a str) -> FlowFuture<'a, Option<String>>;
    /// A selection; `None` when dismissed.
    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
    ) -> FlowFuture<'a, Option<String>>;
    fn notify(&self, message: &str, kind: NotificationType);
}

/// Prepare and submit as the flow sees them.
pub trait FlowBackend: Send + Sync {
    /// A draft for the current session; `Err` is a prepare failure.
    fn prepare(&self) -> Result<PrepareOutcome, String>;
    /// Submits `{draftId, kind, description}`.
    fn submit(&self, input: Value) -> FlowFuture<'_, SubmitResult>;
}

/// The session UI as [`FlowUi`].
pub struct SessionUi<'a>(pub &'a dyn ExtensionUi);

impl FlowUi for SessionUi<'_> {
    fn input<'a>(&'a self, title: &'a str) -> FlowFuture<'a, Option<String>> {
        self.0.input(
            title,
            Some("What happened?"),
            ExtensionUiDialogOptions::default(),
        )
    }

    fn select<'a>(
        &'a self,
        title: &'a str,
        options: &'a [String],
    ) -> FlowFuture<'a, Option<String>> {
        self.0
            .select(title, options, ExtensionUiDialogOptions::default())
    }

    fn notify(&self, message: &str, kind: NotificationType) {
        self.0.notify(message, kind);
    }
}

/// [`Reports`] bound to one prepare request.
pub struct ReportsBackend {
    pub reports: Reports,
    pub request: PrepareRequest,
}

impl FlowBackend for ReportsBackend {
    fn prepare(&self) -> Result<PrepareOutcome, String> {
        Ok(self.reports.prepare(self.request.clone()))
    }

    fn submit(&self, input: Value) -> FlowFuture<'_, SubmitResult> {
        Box::pin(async move {
            let result = self.reports.submit_json(&input).await;
            parse_result(&result)
        })
    }
}

/// A submit result from its JSON form.
pub fn parse_result(value: &Value) -> SubmitResult {
    let field = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
    match field("kind") {
        "sent" => SubmitResult::Sent {
            report_id: field("reportId").to_owned(),
        },
        "saved" => SubmitResult::Saved {
            path: field("path").to_owned(),
        },
        "already-sent" => SubmitResult::AlreadySent,
        _ => SubmitResult::Refused(match field("reason") {
            "secret-detected" => Refusal::SecretDetected,
            "too-large" => Refusal::TooLarge,
            "rate-limited" => Refusal::RateLimited,
            _ => Refusal::Unavailable,
        }),
    }
}

fn join_words(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// The selection title: what is included and where it goes.
pub fn selection_title(draft: &Draft) -> String {
    let words: Vec<&str> = draft
        .sections
        .iter()
        .map(|section| section.id.plain_words())
        .collect();
    let included = format!("This report includes {}.", join_words(&words));
    match draft.destination {
        Destination::Intake => format!(
            "{included} Secrets are removed before sending, and the report is kept for {} days.",
            draft.retention_days()
        ),
        Destination::Local => format!(
            "{included} Uploads are turned off by {DISABLE_ENV}, so the report will be saved on this machine instead."
        ),
    }
}

/// Every section except the description as `## <title>` blocks.
pub fn view_text(draft: &Draft) -> String {
    draft
        .sections
        .iter()
        .filter(|section| section.id != SectionId::Description)
        .map(|section| format!("## {}\n{}", section.id.title(), section.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The single notification for a submit result.
pub fn result_notice(result: &SubmitResult) -> (String, NotificationType) {
    match result {
        SubmitResult::Sent { report_id } => (
            format!("Thanks, your report was sent (report id {report_id})."),
            NotificationType::Info,
        ),
        SubmitResult::Saved { path } => (
            format!("Uploads are off, so the report was saved to {path}."),
            NotificationType::Info,
        ),
        SubmitResult::AlreadySent => (
            "This report was already sent; nothing more to do.".into(),
            NotificationType::Info,
        ),
        SubmitResult::Refused(reason) => {
            let hint = match reason {
                Refusal::SecretDetected => {
                    "the intake found something that looks like a secret. Remove it from your description and try again."
                }
                Refusal::TooLarge => {
                    "the report is too large. Shorten the description or start a new session and try again."
                }
                Refusal::RateLimited => {
                    "too many reports were sent recently. Wait a few minutes and try again."
                }
                Refusal::Unavailable => {
                    "the report service could not be reached or the draft expired. Check your connection and run the command again."
                }
            };
            (
                format!("The report was not sent: {hint}"),
                NotificationType::Error,
            )
        }
    }
}

/// Warning shown when feedback cannot be used in this build.
pub fn unavailable_notice(reason: &str) -> String {
    format!("Feedback cannot be used in this build: {reason}.")
}

/// Runs the command flow for `kind` with the command argument `args`.
pub async fn run(kind: ReportKind, args: &str, ui: &dyn FlowUi, backend: &dyn FlowBackend) {
    let draft = match backend.prepare() {
        Ok(PrepareOutcome::Draft(draft)) => draft,
        Ok(PrepareOutcome::Unavailable { reason }) => {
            ui.notify(&unavailable_notice(&reason), NotificationType::Warning);
            return;
        }
        Err(error) => {
            ui.notify(
                &format!("Could not prepare the report: {error}"),
                NotificationType::Error,
            );
            return;
        }
    };
    let mut description = args.trim().to_owned();
    if description.is_empty() {
        let prompt = match kind {
            ReportKind::Bug => "Describe the bug",
            ReportKind::Feedback | ReportKind::Other => "Your feedback",
        };
        match ui.input(prompt).await {
            Some(answer) if !answer.trim().is_empty() => description = answer.trim().to_owned(),
            _ => return,
        }
    }
    let title = selection_title(&draft);
    let options = vec![SEND.to_owned(), VIEW.to_owned(), CANCEL.to_owned()];
    loop {
        match ui.select(&title, &options).await.as_deref() {
            Some(VIEW) => ui.notify(&view_text(&draft), NotificationType::Info),
            Some(SEND) => {
                let result = backend
                    .submit(json!({
                        "draftId": draft.draft_id,
                        "kind": kind.as_str(),
                        "description": description,
                    }))
                    .await;
                let (message, level) = result_notice(&result);
                ui.notify(&message, level);
                return;
            }
            _ => return,
        }
    }
}
