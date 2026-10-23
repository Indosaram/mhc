//! Onboarding scan wire contract (upstream `scan-contract.ts`).

use serde::{Deserialize, Serialize};

/// Pull: `extension_request` with [`OnboardingScanRequest`]; answers `{scanId}` or unavailable.
pub const ONBOARDING_SCAN_RPC_METHOD: &str = "omo.onboarding.scan";
/// Push: one `extension_event` frame per [`OnboardingScanEvent`], under this name.
pub const ONBOARDING_SCAN_RPC_EVENT: &str = "omo.onboarding.scan.event";
/// Pull: `extension_request` with an apply request; answers [`OnboardingApplyResponse`].
pub const ONBOARDING_APPLY_RPC_METHOD: &str = "omo.onboarding.apply";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OnboardingScanSource {
    #[serde(rename = "detect")]
    Detect,
    #[serde(rename = "providers")]
    Providers,
    #[serde(rename = "credentials")]
    Credentials,
    #[serde(rename = "assets")]
    Assets,
    #[serde(rename = "model-choices")]
    ModelChoices,
    #[serde(rename = "projects")]
    Projects,
    #[serde(rename = "sessions")]
    Sessions,
}

impl OnboardingScanSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Detect => "detect",
            Self::Providers => "providers",
            Self::Credentials => "credentials",
            Self::Assets => "assets",
            Self::ModelChoices => "model-choices",
            Self::Projects => "projects",
            Self::Sessions => "sessions",
        }
    }
}

/// The stages `omo.onboarding.apply` accepts; `detect` only reads, so it has none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OnboardingApplyStage {
    #[serde(rename = "credentials")]
    Credentials,
    #[serde(rename = "assets")]
    Assets,
    #[serde(rename = "providers")]
    Providers,
    #[serde(rename = "model-choices")]
    ModelChoices,
}

impl OnboardingApplyStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Credentials => "credentials",
            Self::Assets => "assets",
            Self::Providers => "providers",
            Self::ModelChoices => "model-choices",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        ONBOARDING_APPLY_STAGES
            .iter()
            .copied()
            .find(|stage| stage.as_str() == value)
    }
}

pub const ONBOARDING_APPLY_STAGES: [OnboardingApplyStage; 4] = [
    OnboardingApplyStage::Credentials,
    OnboardingApplyStage::Assets,
    OnboardingApplyStage::Providers,
    OnboardingApplyStage::ModelChoices,
];

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingScanRequest {
    /// OpenCode custom and catalog providers; off unless asked for.
    #[serde(
        rename = "includeCustomProviders",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub include_custom_providers: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingScanItem {
    pub id: String,
    pub kind: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// On a `sessions` item: the project directory the session ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingScanCounts {
    pub found: usize,
    pub read: usize,
    /// The most a source reads; `None` (null) for a source that reads everything it finds.
    pub cap: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingScanEventKind {
    SourceStarted,
    Item,
    SourceDone,
    SourceFailed,
    Done,
}

/// The source an event belongs to; `all` on the final `done`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OnboardingScanEventSource {
    Source(OnboardingScanSource),
    All(OnboardingScanAll),
}

impl OnboardingScanEventSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source(source) => source.as_str(),
            Self::All(_) => "all",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardingScanAll {
    #[serde(rename = "all")]
    All,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingScanEvent {
    #[serde(rename = "scanId")]
    pub scan_id: String,
    pub kind: OnboardingScanEventKind,
    pub source: OnboardingScanEventSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<OnboardingScanItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<OnboardingScanCounts>,
    #[serde(rename = "elapsedMs")]
    pub elapsed_ms: u64,
    /// On `source_failed`: why the source could not be read; on a failed scan's final `done`: why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingApplyRequest {
    #[serde(rename = "scanId")]
    pub scan_id: String,
    pub stages: Vec<OnboardingApplyStage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingUnavailable {
    pub kind: UnavailableKind,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnavailableKind {
    #[serde(rename = "unavailable")]
    Unavailable,
}

impl OnboardingUnavailable {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            kind: UnavailableKind::Unavailable,
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OnboardingApplyResponse {
    Applied {
        #[serde(rename = "scanId")]
        scan_id: String,
        applied: Vec<OnboardingApplyStage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notices: Option<Vec<String>>,
    },
    Unavailable(OnboardingUnavailable),
    Stale {
        kind: StaleKind,
        reason: String,
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        applied: Option<Vec<OnboardingApplyStage>>,
    },
    Invalid {
        kind: InvalidKind,
        reason: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StaleKind {
    #[serde(rename = "stale")]
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvalidKind {
    #[serde(rename = "invalid")]
    Invalid,
}

impl OnboardingApplyResponse {
    pub fn invalid(reason: impl Into<String>) -> Self {
        Self::Invalid {
            kind: InvalidKind::Invalid,
            reason: reason.into(),
        }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable(OnboardingUnavailable::new(reason))
    }
    pub fn stale(
        reason: impl Into<String>,
        paths: Vec<String>,
        applied: Option<Vec<OnboardingApplyStage>>,
    ) -> Self {
        Self::Stale {
            kind: StaleKind::Stale,
            reason: reason.into(),
            paths,
            applied,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_serialize_with_the_wire_names() {
        let event = OnboardingScanEvent {
            scan_id: "s".into(),
            kind: OnboardingScanEventKind::SourceDone,
            source: OnboardingScanEventSource::Source(OnboardingScanSource::ModelChoices),
            item: None,
            counts: Some(OnboardingScanCounts {
                found: 1,
                read: 1,
                cap: None,
            }),
            elapsed_ms: 3,
            reason: None,
        };
        assert_eq!(
            serde_json::to_value(&event).ok(),
            Some(
                json!({"scanId":"s","kind":"source_done","source":"model-choices","counts":{"found":1,"read":1,"cap":null},"elapsedMs":3})
            )
        );
        let done = OnboardingScanEvent {
            source: OnboardingScanEventSource::All(OnboardingScanAll::All),
            kind: OnboardingScanEventKind::Done,
            counts: None,
            ..event
        };
        assert_eq!(
            serde_json::to_value(&done)
                .ok()
                .map(|v| v["source"].clone()),
            Some(json!("all"))
        );
    }

    #[test]
    fn apply_responses_serialize_with_the_wire_shape() {
        assert_eq!(
            serde_json::to_value(OnboardingApplyResponse::invalid("x")).ok(),
            Some(json!({"kind":"invalid","reason":"x"}))
        );
        assert_eq!(
            serde_json::to_value(OnboardingApplyResponse::stale("r", vec!["p".into()], None)).ok(),
            Some(json!({"kind":"stale","reason":"r","paths":["p"]}))
        );
        assert_eq!(
            OnboardingApplyStage::parse("model-choices"),
            Some(OnboardingApplyStage::ModelChoices)
        );
        assert_eq!(OnboardingApplyStage::parse("detect"), None);
    }
}
