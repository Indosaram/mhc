//! Understanding wire contract (upstream `understand-contract.ts`).

use memory_core::facts::extraction::FactsExtractionRecord;
use serde::{Deserialize, Serialize};

/// Pull: request with `scanId`, `model` and `consent: true`; answers `{understandId}`, unavailable or invalid.
pub const ONBOARDING_UNDERSTAND_RPC_METHOD: &str = "omo.onboarding.understand";
/// Push: one `extension_event` frame per [`OnboardingUnderstandEvent`], under this name.
pub const ONBOARDING_UNDERSTAND_RPC_EVENT: &str = "omo.onboarding.understand.event";
/// Pull: `{understandId, project}`; the project's step is cancelled or never started.
pub const ONBOARDING_UNDERSTAND_SKIP_RPC_METHOD: &str = "omo.onboarding.understand.skip";
/// Pull: `{understandId}`; the run ends with the candidates it already has.
pub const ONBOARDING_UNDERSTAND_STOP_RPC_METHOD: &str = "omo.onboarding.understand.stop";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnboardingUnderstandBudgetLimits {
    /// `record_fact` tool calls across the run, rejected ones included; NOT model calls. Model calls are
    /// bounded separately: one per step plus at most one retry of a failed step.
    pub tool_calls: u32,
    pub read_bytes: usize,
    pub total_ms: u64,
    pub step_ms: u64,
    pub max_facts: usize,
    pub min_source_sessions: usize,
}

pub const ONBOARDING_UNDERSTAND_BUDGET: OnboardingUnderstandBudgetLimits =
    OnboardingUnderstandBudgetLimits {
        tool_calls: 24,
        read_bytes: 240 * 1024,
        total_ms: 90_000,
        step_ms: 15_000,
        max_facts: 12,
        min_source_sessions: 2,
    };

/// Request of `omo.onboarding.understand`; `consent` must be `true`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingUnderstandRequest {
    #[serde(rename = "scanId")]
    pub scan_id: String,
    /// `provider/model`: the caller's default model.
    pub model: String,
    pub consent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingUnderstandSkipRequest {
    #[serde(rename = "understandId")]
    pub understand_id: String,
    pub project: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingUnderstandStopRequest {
    #[serde(rename = "understandId")]
    pub understand_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingUnderstandStep {
    pub id: String,
    /// Human-readable, e.g. "Read 12 recent sessions in omo".
    pub label: String,
    /// The project directory; absent on the step that reads single-session projects together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub sessions: usize,
}

/// A proposed fact: never applied here; the apply stage writes it through the engine memory path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingFactCandidate {
    pub id: String,
    pub fact: FactsExtractionRecord,
    /// Session keys of the digests the model read when it recorded the fact; at least two.
    pub sources: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingUnderstandBudgetKind {
    ToolCalls,
    ReadBytes,
    TotalMs,
    StepMs,
    MaxFacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingUnderstandEventKind {
    StepStarted,
    StepDone,
    StepFailed,
    Fact,
    Budget,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnderstandFallback {
    #[serde(rename = "scan_only")]
    ScanOnly,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingUnderstandEvent {
    #[serde(rename = "understandId")]
    pub understand_id: String,
    #[serde(rename = "scanId")]
    pub scan_id: String,
    pub kind: OnboardingUnderstandEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<OnboardingUnderstandStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fact: Option<OnboardingFactCandidate>,
    /// On `budget`: the cap that was reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<OnboardingUnderstandBudgetKind>,
    /// Bytes of digests and AGENTS.md read so far in this run.
    #[serde(rename = "bytesRead")]
    pub bytes_read: usize,
    /// `record_fact` calls so far in this run.
    #[serde(rename = "toolCalls")]
    pub tool_calls: u32,
    #[serde(rename = "elapsedMs")]
    pub elapsed_ms: u64,
    /// On `step_failed`: why the model call failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// On `step_failed`: the step runs once more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrying: Option<bool>,
    /// On `step_done`: the project was skipped; its facts were dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<bool>,
    /// On `done`: every candidate, in the order found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<OnboardingFactCandidate>>,
    /// On `done`: the model failed twice; onboarding continues with the scan only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<UnderstandFallback>,
    /// On `done`: ended by `omo.onboarding.understand.stop`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_matches_the_contract() {
        let budget = ONBOARDING_UNDERSTAND_BUDGET;
        assert_eq!(
            (
                budget.tool_calls,
                budget.read_bytes,
                budget.total_ms,
                budget.step_ms,
                budget.max_facts,
                budget.min_source_sessions
            ),
            (24, 240 * 1024, 90_000, 15_000, 12, 2)
        );
    }

    #[test]
    fn budget_kinds_use_snake_case_wire_names() {
        assert_eq!(
            serde_json::to_value(OnboardingUnderstandBudgetKind::ReadBytes).ok(),
            Some(serde_json::json!("read_bytes"))
        );
        assert_eq!(
            serde_json::to_value(OnboardingUnderstandEventKind::StepStarted).ok(),
            Some(serde_json::json!("step_started"))
        );
    }
}
