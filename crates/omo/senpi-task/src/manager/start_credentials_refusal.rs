//! `manager/start-credentials-refusal.ts`.

use crate::manager::start_failure_cause::{StartCause, StartCauseClass};
use crate::runners::{RunnerFailure, RunnerFailureKind};
use crate::state::{ResolvedModelRecord, TaskRecord};

/// The start refusal that says the child's provider cannot authenticate (#10806), or `None` for any
/// other failure.
pub fn start_credentials_refusal(failure: &RunnerFailure) -> Option<&StartCause> {
    if failure.kind != RunnerFailureKind::ChildPromptFailed {
        return None;
    }
    failure
        .cause
        .as_deref()
        .filter(|cause| cause.cause_class == StartCauseClass::CredentialsMissing)
}

/// The chain left after a credential refusal: every rung on the refusing provider would be refused
/// the same way.
pub fn chain_without_refusing_provider(
    record: &TaskRecord,
    refusal: &StartCause,
) -> Vec<ResolvedModelRecord> {
    let chain = record.fallback_models.clone().unwrap_or_default();
    let provider = refusal.provider.as_deref().or_else(|| {
        record
            .resolved_model
            .as_ref()
            .map(|model| model.provider.as_str())
    });
    match provider {
        None => chain,
        Some(provider) => chain
            .into_iter()
            .filter(|model| model.provider != provider)
            .collect(),
    }
}
