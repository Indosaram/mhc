//! Working-agreement ledger types (`system/agreements.md`).

use serde::{Deserialize, Serialize};

pub const AGREEMENTS_PATH: &str = "system/agreements.md";
pub const AGREEMENTS_DESCRIPTION: &str =
    "Agreements - standing working agreements with my person, with how often each was broken";
pub const AGREEMENT_TEXT_MAX_CHARS: usize = 240;

/// Whether an agreement holds everywhere or only in one project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgreementScope {
    Global,
    Project,
}

impl AgreementScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

/// Who wrote an agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgreementAuthor {
    Human,
    Agent,
}

impl AgreementAuthor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgreementStatus {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "needs-human")]
    NeedsHuman,
}

impl AgreementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::NeedsHuman => "needs-human",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreementItem {
    pub id: String,
    pub scope: AgreementScope,
    pub by: AgreementAuthor,
    pub text: String,
    pub n: u64,
    pub since: Option<String>,
    pub last: Option<String>,
    pub status: AgreementStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgreementsPolicy {
    pub needs_human_threshold: u64,
    pub max_items: usize,
}

pub const DEFAULT_AGREEMENTS_POLICY: AgreementsPolicy = AgreementsPolicy {
    needs_human_threshold: 3,
    max_items: 64,
};

impl Default for AgreementsPolicy {
    fn default() -> Self {
        DEFAULT_AGREEMENTS_POLICY
    }
}

/// An agreement as shown to the facts extractor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsKnownAgreement {
    pub id: String,
    pub scope: AgreementScope,
    pub by: AgreementAuthor,
    pub text: String,
    pub n: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgreementSourceEvent {
    pub session_id: String,
    pub message_id: String,
}

/// One agreement record emitted by the facts extractor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum AgreementExtractionRecord {
    New {
        applies: AgreementScope,
        text: String,
        date: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<AgreementSourceEvent>,
    },
    Reaffirm {
        id: String,
        date: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<AgreementSourceEvent>,
    },
}
