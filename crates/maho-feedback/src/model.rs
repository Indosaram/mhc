//! Wire shapes of the feedback flow: sections, the draft, report kinds and submit results
//! (spec sections 4.1 and 5.2). Field names and enumerated values are interface facts.

use jiff::Timestamp;
use serde_json::{Value, json};

/// How long the intake keeps a report (spec 4.1).
pub const RETENTION_DAYS: u32 = 90;

/// Report sections, in the fixed draft order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SectionId {
    Description,
    Identity,
    Environment,
    Session,
    Logs,
    Crash,
    Config,
}

impl SectionId {
    /// Every section in draft order.
    pub const ORDER: [Self; 7] = [
        Self::Description,
        Self::Identity,
        Self::Environment,
        Self::Session,
        Self::Logs,
        Self::Crash,
        Self::Config,
    ];

    /// The wire id.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Description => "description",
            Self::Identity => "identity",
            Self::Environment => "environment",
            Self::Session => "session",
            Self::Logs => "logs",
            Self::Crash => "crash",
            Self::Config => "config",
        }
    }

    /// From a wire id.
    pub fn parse(id: &str) -> Option<Self> {
        Self::ORDER
            .into_iter()
            .find(|section| section.as_str() == id)
    }

    /// The human heading.
    pub fn title(self) -> &'static str {
        match self {
            Self::Description => "Description",
            Self::Identity => "Identity",
            Self::Environment => "Environment",
            Self::Session => "Session transcript",
            Self::Logs => "Recent log events",
            Self::Crash => "Crash details",
            Self::Config => "Configuration",
        }
    }

    /// Plain-words name used when telling the user what a report includes.
    pub fn plain_words(self) -> &'static str {
        match self {
            Self::Description => "your description",
            Self::Identity => "this device's name",
            Self::Environment => "version and system details",
            Self::Session => "the conversation of this session",
            Self::Logs => "recent log events",
            Self::Crash => "crash details",
            Self::Config => "your settings with secrets blanked",
        }
    }

    /// Whether the section text goes through the upload redactor.
    pub fn redacted(self) -> bool {
        self != Self::Identity
    }
}

/// One draft section; `text` is byte-for-byte what is uploaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: SectionId,
    pub text: String,
    pub redacted_count: usize,
}

impl Section {
    /// UTF-8 length of the text.
    pub fn bytes(&self) -> usize {
        self.text.len()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id.as_str(),
            "title": self.id.title(),
            "text": self.text,
            "redactedCount": self.redacted_count,
            "bytes": self.bytes(),
        })
    }
}

/// Where a submitted draft goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// Posted to the report intake.
    Intake,
    /// Uploads are off; saved to a private local file.
    Local,
}

impl Destination {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intake => "intake",
            Self::Local => "local",
        }
    }
}

/// The `client` object of a report body, captured when the draft is prepared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    /// `desktop` or `cli`.
    pub app: String,
    pub product_version: String,
    pub host_version: String,
    pub os: String,
    pub arch: String,
    pub locale: String,
    pub channel: String,
    pub device_name: Option<String>,
}

impl ClientInfo {
    pub fn to_json(&self) -> Value {
        let mut client = json!({
            "app": self.app,
            "omo": self.product_version,
            "senpi": self.host_version,
            "os": self.os,
            "arch": self.arch,
            "locale": self.locale,
            "channel": self.channel,
        });
        if let (Some(name), Some(object)) = (&self.device_name, client.as_object_mut()) {
            object.insert("deviceName".into(), Value::String(name.clone()));
        }
        client
    }
}

/// A prepared report awaiting submit.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub draft_id: String,
    /// In draft order; always starts with an empty `description`.
    pub sections: Vec<Section>,
    pub destination: Destination,
    pub signed_in: bool,
    pub client: ClientInfo,
    pub created_at: Timestamp,
}

impl Draft {
    pub fn retention_days(&self) -> u32 {
        RETENTION_DAYS
    }

    pub fn total_bytes(&self) -> usize {
        self.sections.iter().map(Section::bytes).sum()
    }

    pub fn redacted_count(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.redacted_count)
            .sum()
    }

    pub fn section(&self, id: SectionId) -> Option<&Section> {
        self.sections.iter().find(|section| section.id == id)
    }

    /// The draft as returned by the prepare RPC.
    pub fn to_json(&self) -> Value {
        json!({
            "draftId": self.draft_id,
            "destination": self.destination.as_str(),
            "retentionDays": self.retention_days(),
            "signedIn": self.signed_in,
            "totalBytes": self.total_bytes(),
            "redactedCount": self.redacted_count(),
            "sections": self.sections.iter().map(Section::to_json).collect::<Vec<_>>(),
        })
    }
}

/// Result of preparing a draft.
#[derive(Clone, Debug, PartialEq)]
pub enum PrepareOutcome {
    Draft(Box<Draft>),
    /// Feedback cannot be used (no upload redactor in this build).
    Unavailable {
        reason: String,
    },
}

impl PrepareOutcome {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Draft(draft) => draft.to_json(),
            Self::Unavailable { reason } => json!({"kind": "unavailable", "reason": reason}),
        }
    }
}

/// The kind of a submitted report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportKind {
    Feedback,
    Bug,
    Other,
}

impl ReportKind {
    /// `feedback` and `other` are kept; anything else is a bug report.
    pub fn parse(kind: Option<&str>) -> Self {
        match kind {
            Some("feedback") => Self::Feedback,
            Some("other") => Self::Other,
            _ => Self::Bug,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Feedback => "feedback",
            Self::Bug => "bug",
            Self::Other => "other",
        }
    }
}

/// Why a submit did not go through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    SecretDetected,
    TooLarge,
    RateLimited,
    Unavailable,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SecretDetected => "secret-detected",
            Self::TooLarge => "too-large",
            Self::RateLimited => "rate-limited",
            Self::Unavailable => "unavailable",
        }
    }

    /// Whether the draft stays stored so it can be submitted again.
    pub fn keeps_draft(self) -> bool {
        matches!(self, Self::RateLimited | Self::Unavailable)
    }
}

/// Result of a submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitResult {
    Sent { report_id: String },
    Saved { path: String },
    AlreadySent,
    Refused(Refusal),
}

impl SubmitResult {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Sent { report_id } => json!({"kind": "sent", "reportId": report_id}),
            Self::Saved { path } => json!({"kind": "saved", "path": path}),
            Self::AlreadySent => json!({"kind": "already-sent"}),
            Self::Refused(reason) => json!({"kind": "refused", "reason": reason.as_str()}),
        }
    }

    /// Whether the draft is removed from the store after this result.
    pub fn removes_draft(&self) -> bool {
        match self {
            Self::Refused(reason) => !reason.keeps_draft(),
            _ => true,
        }
    }
}
