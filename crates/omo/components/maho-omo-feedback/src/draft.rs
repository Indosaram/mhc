//! Port of `feedback/draft.ts`: the exact report that leaves the machine.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeedbackSectionId {
    Description,
    Identity,
    Environment,
    Session,
    Logs,
    Crash,
    Config,
}

/// Fixed section order of every draft.
pub const FEEDBACK_SECTION_IDS: [FeedbackSectionId; 7] = [
    FeedbackSectionId::Description,
    FeedbackSectionId::Identity,
    FeedbackSectionId::Environment,
    FeedbackSectionId::Session,
    FeedbackSectionId::Logs,
    FeedbackSectionId::Crash,
    FeedbackSectionId::Config,
];

impl FeedbackSectionId {
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

    pub fn parse(value: &str) -> Option<Self> {
        FEEDBACK_SECTION_IDS
            .into_iter()
            .find(|id| id.as_str() == value)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Description => "Description",
            Self::Identity => "Who you are",
            Self::Environment => "App and system",
            Self::Session => "This conversation",
            Self::Logs => "Logs and recent errors",
            Self::Crash => "Crash report",
            Self::Config => "Settings",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackKind {
    Bug,
    Feedback,
    Other,
}

impl FeedbackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Feedback => "feedback",
            Self::Other => "other",
        }
    }

    /// Anything but `feedback` and `other` is a bug, like upstream's submit shell.
    pub fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("feedback") => Self::Feedback,
            Some("other") => Self::Other,
            _ => Self::Bug,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackDestination {
    Intake,
    Local,
}

impl FeedbackDestination {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intake => "intake",
            Self::Local => "local",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackDraftSection {
    pub id: FeedbackSectionId,
    pub title: &'static str,
    pub text: String,
    pub redacted_count: usize,
    pub bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackDraft {
    pub draft_id: String,
    pub sections: Vec<FeedbackDraftSection>,
    pub destination: FeedbackDestination,
    pub retention_days: u32,
    pub signed_in: bool,
    pub total_bytes: usize,
    pub redacted_count: usize,
}

impl FeedbackDraft {
    /// The wire shape the `omo.feedback.prepare` RPC returns (upstream's camelCase object).
    pub fn to_json(&self) -> Value {
        json!({
            "draftId": self.draft_id,
            "sections": self.sections.iter().map(|section| json!({
                "id": section.id.as_str(),
                "title": section.title,
                "text": section.text,
                "redactedCount": section.redacted_count,
                "bytes": section.bytes,
            })).collect::<Vec<_>>(),
            "destination": self.destination.as_str(),
            "retentionDays": self.retention_days,
            "signedIn": self.signed_in,
            "totalBytes": self.total_bytes,
            "redactedCount": self.redacted_count,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectedSection {
    pub id: FeedbackSectionId,
    pub text: String,
    pub verbatim: bool,
}

impl CollectedSection {
    pub fn new(id: FeedbackSectionId, text: impl Into<String>) -> Self {
        Self {
            id,
            text: text.into(),
            verbatim: false,
        }
    }

    pub fn verbatim(id: FeedbackSectionId, text: impl Into<String>) -> Self {
        Self {
            id,
            text: text.into(),
            verbatim: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redaction {
    pub text: String,
    pub count: usize,
}

/// The one upload redactor (the host's `redactForUpload`). Must be idempotent.
pub type UploadRedactor = Arc<dyn Fn(&str) -> Redaction + Send + Sync>;

pub const REPORT_RETENTION_DAYS: u32 = 90;

pub fn byte_length(text: &str) -> usize {
    text.len()
}

pub struct BuildDraftOptions<'a> {
    pub draft_id: String,
    pub collected: &'a [CollectedSection],
    pub redact: &'a UploadRedactor,
    pub destination: FeedbackDestination,
    pub signed_in: bool,
}

/// Every section except a `verbatim` one goes through the one redactor; sections come out in the
/// fixed order whatever order they were collected in. The description is always present.
pub fn build_draft(options: BuildDraftOptions<'_>) -> FeedbackDraft {
    let by_id: HashMap<FeedbackSectionId, &CollectedSection> = options
        .collected
        .iter()
        .map(|section| (section.id, section))
        .collect();
    let empty = CollectedSection::new(FeedbackSectionId::Description, "");
    let mut sections = Vec::new();
    for id in FEEDBACK_SECTION_IDS {
        let collected = match by_id.get(&id) {
            Some(section) => *section,
            None if id == FeedbackSectionId::Description => &empty,
            None => continue,
        };
        let redacted = if collected.verbatim {
            Redaction {
                text: collected.text.clone(),
                count: 0,
            }
        } else {
            (options.redact)(&collected.text)
        };
        let bytes = byte_length(&redacted.text);
        sections.push(FeedbackDraftSection {
            id,
            title: id.title(),
            text: redacted.text,
            redacted_count: redacted.count,
            bytes,
        });
    }
    let total_bytes = sections.iter().map(|section| section.bytes).sum();
    let redacted_count = sections.iter().map(|section| section.redacted_count).sum();
    FeedbackDraft {
        draft_id: options.draft_id,
        destination: options.destination,
        retention_days: REPORT_RETENTION_DAYS,
        signed_in: options.signed_in,
        sections,
        total_bytes,
        redacted_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker_redactor() -> UploadRedactor {
        Arc::new(|text: &str| {
            let count = text.matches("SECRET").count();
            Redaction {
                text: text.replace("SECRET", "<redacted>"),
                count,
            }
        })
    }

    #[test]
    fn orders_sections_and_always_includes_empty_description() {
        let redact = marker_redactor();
        let collected = vec![
            CollectedSection::new(FeedbackSectionId::Config, "{}"),
            CollectedSection::new(FeedbackSectionId::Logs, "line SECRET"),
            CollectedSection::verbatim(FeedbackSectionId::Identity, "device    SECRET-host"),
        ];
        let draft = build_draft(BuildDraftOptions {
            draft_id: "drf_1".into(),
            collected: &collected,
            redact: &redact,
            destination: FeedbackDestination::Intake,
            signed_in: false,
        });
        let ids: Vec<&str> = draft
            .sections
            .iter()
            .map(|section| section.id.as_str())
            .collect();
        assert_eq!(ids, ["description", "identity", "logs", "config"]);
        assert_eq!(draft.sections[0].text, "");
        assert_eq!(draft.sections[1].text, "device    SECRET-host");
        assert_eq!(draft.sections[1].redacted_count, 0);
        assert_eq!(draft.sections[2].text, "line <redacted>");
        assert_eq!(draft.redacted_count, 1);
        assert_eq!(draft.retention_days, 90);
        assert_eq!(
            draft.total_bytes,
            draft
                .sections
                .iter()
                .map(|section| section.bytes)
                .sum::<usize>()
        );
        assert_eq!(draft.to_json()["destination"], "intake");
    }
}
