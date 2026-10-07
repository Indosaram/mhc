//! Memory child model chain (latest `memory-child-model-chain.ts`).
//!
//! The chain fields a memory child's `ChildSpec` override carries: the bare primary selector, the
//! in-category fallback records, and a same-model retry budget short enough that the runtime reaches
//! the fallbacks before a `tool_call` child's deadline. This module ports `childModelChainSpec` and
//! the `Pick<ChildSpec, ...>` projection it returns; the launch sites (`facts-in-process-launch`,
//! `sidecar-model`) spread the projection into a real `ChildSpec` and are their own owners.
//!
//! # Types
//!
//! * `ResolvedModelRecord` / `ResolvedModelSource` are the native `senpi_task::state` types, used
//!   verbatim - no duplicate bridge record is invented.
//! * `ChildModelChainSpec` is upstream's `Pick<ChildSpec, "selectedModel" | "fallbackModels" |
//!   "retry">`. Rust has no `Pick`, so the projection is its own struct; every field is the exact
//!   native `ChildSpec` field type (`Option<String>`, `Option<Vec<ResolvedModelRecord>>`,
//!   `Option<ChildRetryOverride>`).
//! * `ChildRetryOverride` (upstream `runners/in-process/runtime-fallback-settings.ts`) is NOT yet
//!   ported: `senpi_task` has no such type and `ChildSpec` has no `retry` field. This module
//!   references the native type it must resolve to and carries the exact producer demand in its
//!   receipt; it neither stubs the retry nor omits it.

use senpi_task::runners::in_process::ChildRetryOverride;
use senpi_task::state::{ResolvedModelRecord, ResolvedModelSource};

use crate::worker::memory_model_attempts::ReflectionModelCandidate;

/// One same-model retry per rung. A `tool_call` child has 90s; the engine's default budget spends
/// ~62s of exponential backoff on the primary alone, so the chain would never be reached in time.
pub const MEMORY_CHILD_SAME_MODEL_RETRIES: u32 = 1;

/// `Pick<ChildSpec, "selectedModel" | "fallbackModels" | "retry">`: the model-selection fields a
/// memory child's launch override carries, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildModelChainSpec {
    /// `ChildSpec.selectedModel`: the bare primary selector, kept as the whole string.
    pub selected_model: Option<String>,
    /// `ChildSpec.fallbackModels`: the in-category fallback records, in order; absent for a
    /// single-model category.
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    /// `ChildSpec.retry`: the same-model retry budget; absent for a single-model category.
    pub retry: Option<ChildRetryOverride>,
}

/// The chain inputs: the resolved primary selector and the resolved in-category fallback candidates.
pub struct ChildModelChainInput<'a> {
    pub model: &'a str,
    pub fallbacks: &'a [ReflectionModelCandidate],
}

/// `childModelChainSpec`: the model-selection projection a memory child's `ChildSpec` carries.
///
/// Zero fallbacks is the single-model case: only `selected_model` is set, with no fallback list and
/// no retry override, so the engine's own defaults stand.
pub fn child_model_chain_spec(input: ChildModelChainInput<'_>) -> ChildModelChainSpec {
    if input.fallbacks.is_empty() {
        return ChildModelChainSpec {
            selected_model: Some(input.model.to_string()),
            fallback_models: None,
            retry: None,
        };
    }
    ChildModelChainSpec {
        selected_model: Some(input.model.to_string()),
        fallback_models: Some(input.fallbacks.iter().map(fallback_record).collect()),
        retry: Some(ChildRetryOverride {
            max_retries: Some(MEMORY_CHILD_SAME_MODEL_RETRIES),
            base_delay_ms: None,
        }),
    }
}

/// One candidate becomes a `ResolvedModelRecord`: the provider is the text before the FIRST slash
/// (the whole string when there is none) and the model id keeps the rest, so a nested-slash model id
/// is never split again. The display is the candidate's own full string, the source is always
/// `category`, and a candidate's thinking level rides on the canonical `reasoning` field.
fn fallback_record(candidate: &ReflectionModelCandidate) -> ResolvedModelRecord {
    let (provider, model_id) = match candidate.model.split_once('/') {
        Some((provider, rest)) => (provider.to_string(), rest.to_string()),
        None => (candidate.model.clone(), String::new()),
    };
    ResolvedModelRecord {
        provider,
        model_id,
        display: candidate.model.clone(),
        source: ResolvedModelSource::Category,
        variant: None,
        reasoning_effort: None,
        reasoning: candidate.thinking.clone(),
    }
}
