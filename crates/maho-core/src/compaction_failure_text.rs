//! Port of senpi `packages/coding-agent/src/core/compaction-failure-text.ts`.

use std::sync::LazyLock;

use maho_ai::utils::provider_failure_description::{
    describe_provider_failure_for_user, strip_turn_retry_suppression_prefix,
};
use maho_ai::utils::retry::{ProviderStallDescriptionOptions, is_retryable_error_message};
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionFailureKind {
    Compaction,
    AutoCompaction,
    OverflowRecovery,
}

impl CompactionFailureKind {
    fn lead(self) -> &'static str {
        match self {
            Self::Compaction => "Compaction failed",
            Self::AutoCompaction => "Auto-compaction failed",
            Self::OverflowRecovery => "Context overflow recovery failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionFailureStep {
    Provider,
    Resend,
    LargerContext,
    NothingToCompact,
    CompactAgain,
    SelfExplained,
}

static NOTHING_TO_COMPACT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^(?:Nothing to compact|Already compacted)\b").ok());
static SELF_EXPLAINED: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^Compaction (?:rejected|cancelled)\b").ok());

fn step_text(step: CompactionFailureStep) -> Option<&'static str> {
    match step {
        CompactionFailureStep::Resend => Some(
            "The conversation is larger than the model's context, and compacting it hit a temporary provider error. Send the message again; if it keeps failing, switch to a larger-context model.",
        ),
        CompactionFailureStep::LargerContext => Some(
            "The conversation is larger than the model's context, and compacting it did not finish. Start a new session, or switch to a larger-context model.",
        ),
        CompactionFailureStep::NothingToCompact => Some(
            "There is nothing older to summarize yet, so the context was left as it is. No action is needed.",
        ),
        CompactionFailureStep::CompactAgain => Some(
            "The summary of the earlier conversation could not be written. Compact again; if it keeps failing, start a new session or choose another model.",
        ),
        CompactionFailureStep::Provider | CompactionFailureStep::SelfExplained => None,
    }
}

fn provider_text(cause: &str, model: Option<&str>) -> Option<String> {
    let options = ProviderStallDescriptionOptions {
        model: model.map(str::to_owned),
        ..Default::default()
    };
    if let Some(text) = describe_provider_failure_for_user(Some(cause), &options) {
        return Some(text);
    }
    if crate::retry_fallback::usage_limit::usage_limit_scope_text(Some(cause))
        == Some(maho_ai::types::UsageLimitScope::Account)
        && let Some(provider) = model
            .and_then(|m| m.split('/').next())
            .filter(|p| !p.is_empty())
    {
        return Some(
            crate::retry_fallback::usage_limit::account_usage_limit_error(provider, Some(cause)),
        );
    }
    None
}

/// Which step a failure names: a transient provider error during overflow recovery is worth resending (senpi#4329).
pub fn compaction_failure_step(
    kind: CompactionFailureKind,
    cause: &str,
    model: Option<&str>,
) -> CompactionFailureStep {
    if SELF_EXPLAINED.as_ref().is_some_and(|re| re.is_match(cause)) {
        return CompactionFailureStep::SelfExplained;
    }
    if provider_text(cause, model).is_some() {
        return CompactionFailureStep::Provider;
    }
    if kind == CompactionFailureKind::OverflowRecovery {
        return if is_retryable_error_message(cause) {
            CompactionFailureStep::Resend
        } else {
            CompactionFailureStep::LargerContext
        };
    }
    if NOTHING_TO_COMPACT
        .as_ref()
        .is_some_and(|re| re.is_match(cause))
    {
        return CompactionFailureStep::NothingToCompact;
    }
    CompactionFailureStep::CompactAgain
}

/// The `compaction_end` failure text: the lead, the cause class with one next step that works on every
/// surface, then the raw cause as a diagnostic (senpi#4329).
pub fn format_compaction_failure(
    kind: CompactionFailureKind,
    error: &str,
    model: Option<&str>,
) -> String {
    let lead = kind.lead();
    let stripped = strip_turn_retry_suppression_prefix(error);
    let cause = stripped.trim();
    if cause.is_empty() {
        return format!("{lead}.");
    }
    let step = compaction_failure_step(kind, cause, model);
    if step == CompactionFailureStep::SelfExplained {
        return format!("{lead}: {cause}");
    }
    let explanation = match step {
        CompactionFailureStep::Provider => provider_text(cause, model).unwrap_or_default(),
        other => step_text(other).unwrap_or_default().to_owned(),
    };
    format!("{lead}: {explanation} Cause: {cause}")
}
