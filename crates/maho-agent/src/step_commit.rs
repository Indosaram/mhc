//! Port of senpi `packages/agent/src/step-commit.ts`.
//!
//! The durable step commit a hosted brain host installs through `AgentLoopConfig::commit_step`.

use std::sync::Arc;

use maho_ai::types::{AssistantMessage, BoxFuture, ToolResultMessage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepCommitFailure {
    AlreadyCommitted,
    Fenced,
    Generation,
    EpochUnverified,
    StorageUnavailable,
    AmbiguousUnresolved,
}

impl StepCommitFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyCommitted => "already_committed",
            Self::Fenced => "fenced",
            Self::Generation => "generation",
            Self::EpochUnverified => "epoch_unverified",
            Self::StorageUnavailable => "storage_unavailable",
            Self::AmbiguousUnresolved => "ambiguous_unresolved",
        }
    }
}

/// A hosted step commit failed; the step's message is not durable and must not be acted on.
#[derive(Debug, Clone, thiserror::Error)]
#[error("step commit failed: {}", code.as_str())]
pub struct StepCommitError {
    pub step_id: String,
    pub code: StepCommitFailure,
    #[source]
    pub cause: Option<Arc<dyn std::error::Error + Send + Sync>>,
}

impl PartialEq for StepCommitError {
    fn eq(&self, other: &Self) -> bool {
        self.step_id == other.step_id && self.code == other.code
    }
}

impl Eq for StepCommitError {}

impl StepCommitError {
    pub fn new(step_id: impl Into<String>, code: StepCommitFailure) -> Self {
        Self {
            step_id: step_id.into(),
            code,
            cause: None,
        }
    }

    pub fn with_cause(
        step_id: impl Into<String>,
        code: StepCommitFailure,
        cause: Arc<dyn std::error::Error + Send + Sync>,
    ) -> Self {
        Self {
            step_id: step_id.into(),
            code,
            cause: Some(cause),
        }
    }
}

/// One model step as the loop finished it: its terminal assistant message and the provider tool results emitted for it.
#[derive(Debug, Clone)]
pub struct StepCommit {
    pub message: AssistantMessage,
    pub provider_tool_results: Vec<ToolResultMessage>,
}

/// Makes one finished model step durable before the loop acts on it. A rejection leaves the loop with no further
/// events and no tool dispatch for that step.
pub type StepCommitFn =
    Arc<dyn Fn(StepCommit) -> BoxFuture<'static, Result<(), StepCommitError>> + Send + Sync>;

/// True for a `StepCommitError` (matching `isStepCommitError` in senpi `step-commit.ts`).
pub fn is_step_commit_error(err: &(dyn std::error::Error + 'static)) -> bool {
    err.downcast_ref::<StepCommitError>().is_some()
}
