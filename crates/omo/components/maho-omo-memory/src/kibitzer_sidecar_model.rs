//! The resident Kibitzer sidecar's typed start refusal and its configuration classifier
//! (latest `kibitzer/sidecar-model.ts`: the `KibitzerSidecarStartError` / `kibitzerConfigurationFailure`
//! half only).
//!
//! Upstream's `createKibitzerSidecarChildStarter` throws a typed `KibitzerSidecarStartError` whose
//! `code` is a closed union of six start stages, and `kibitzerConfigurationFailure` classifies that
//! error by its STRUCTURED fields - never the message text - into a permanent configuration state
//! (`category_unavailable` / `beyond_category`) or `undefined` (a transient failure).
//!
//! # Scope
//!
//! This module owns the typed start error, the configuration classifier, the model resolver
//! (`resolveKibitzerSidecarModel`) AND the ChildSpec builder (`buildKibitzerSidecarSpec`). The
//! starter port (`createKibitzerSidecarChildStarter`) is a separate owner, as are the spawner ABI
//! change and the native category resolution that constructs these errors. The builder maps the
//! resolver's chain onto the native `ChildSpec`; it touches neither the CLI nor the child session.
//!
//! # C6 as a typed boundary
//!
//! Upstream takes `error: unknown` and rejects anything that is not a `KibitzerSidecarStartError`.
//! The Rust classifier takes `&KibitzerSidecarStartError`, so a non-start error cannot even reach
//! it: C6 is enforced by the type system rather than by a runtime branch.

use std::fmt;

use serde_json::Value;
use senpi_task::category::{CategoryResolutionResult, ResolveCategoryOptions, resolve_category};
use senpi_task::host::{HostError, SenpiModelRegistry};

use crate::kibitzer_sidecar_connected_order::{
    KibitzerCandidateOrder, KibitzerCandidateOrderInput, order_kibitzer_candidates_by_connection,
};
use crate::kibitzer_sidecar_outcome::{KibitzerWakeConfiguration, KibitzerWakeConfigurationCause};
use crate::memory_child_model_chain::{ChildModelChainInput, ChildModelChainSpec, child_model_chain_spec};
use crate::worker::memory_model_attempts::ReflectionModelCandidate;
use crate::worker::resolve_model::{ReflectionModelResolution, resolve_reflection_model};
use senpi_task::runners::in_process::child_options::HostHandle;
use senpi_task::runners::in_process::shared_tool_filter::ChildToolRef;
use senpi_task::runners::in_process::{ChildCompletionPolicy, ChildPromptEnvelope, ChildSpec};

use crate::kibitzer_prompt_blocks::KIBITZER_SIDECAR_TOOL_NAMES;

/// The stage that refused to start a resident child (upstream `KibitzerSidecarStartCode`).
///
/// A closed set: two model-resolution stages are permanent configuration states the classifier
/// answers non-diagnostically; the other four are transient start failures that feed the failure
/// streak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerSidecarStartCode {
    /// No registry snapshot was captured, so the resolver had no honest answer (transient).
    RegistrySnapshotUnavailable,
    /// The pinned category's chain has no connected provider (configuration).
    CategoryUnavailable,
    /// The model resolved only OUTSIDE the pinned category, which the advisor refuses (configuration).
    BeyondCategory,
    /// The persona asset could not be read (transient).
    PersonaUnavailable,
    /// The in-process task runtime could not be loaded (transient).
    RuntimeUnavailable,
    /// The child session could not be created (transient).
    SessionCreateFailed,
}

/// A child that could not be started, named by the stage that refused (upstream
/// `KibitzerSidecarStartError`).
///
/// `message` is the human-readable reason and is what `Display` yields; the classifier reads only
/// `code`, `category` and `missing_providers`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KibitzerSidecarStartError {
    /// The stage that refused.
    pub code: KibitzerSidecarStartCode,
    /// The pinned recall category a model-unavailable refusal names; `None` for the other stages.
    pub category: Option<String>,
    /// The category chain's providers with no connection, when the resolver knew them.
    pub missing_providers: Option<Vec<String>>,
    /// The human-readable reason (`Display` / `Error::message` contract).
    pub message: String,
}

impl KibitzerSidecarStartError {
    /// A refusal from `code` with the given message and no category/providers attached.
    pub fn new(code: KibitzerSidecarStartCode, message: impl Into<String>) -> Self {
        Self { code, category: None, missing_providers: None, message: message.into() }
    }

    /// Attaches the pinned recall category a model refusal names.
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }

    /// Attaches the category chain's unconnected providers.
    pub fn with_missing_providers(mut self, missing_providers: Vec<String>) -> Self {
        self.missing_providers = Some(missing_providers);
        self
    }
}

impl fmt::Display for KibitzerSidecarStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for KibitzerSidecarStartError {}

/// `kibitzerConfigurationFailure`: a permanent configuration state, not a transient start failure.
///
/// Only `category_unavailable` and `beyond_category`, and only when the refusal names a category,
/// are configuration failures; every other start code (and a model refusal with no category) is
/// `None`. The classifier reads the STRUCTURED fields only - never `message` - so a caller cannot
/// smuggle a configuration state through prose.
pub fn kibitzer_configuration_failure(error: &KibitzerSidecarStartError) -> Option<KibitzerWakeConfiguration> {
    let cause = match error.code {
        KibitzerSidecarStartCode::CategoryUnavailable => KibitzerWakeConfigurationCause::CategoryUnavailable,
        KibitzerSidecarStartCode::BeyondCategory => KibitzerWakeConfigurationCause::BeyondCategory,
        KibitzerSidecarStartCode::RegistrySnapshotUnavailable => return None,
        KibitzerSidecarStartCode::PersonaUnavailable => return None,
        KibitzerSidecarStartCode::RuntimeUnavailable => return None,
        KibitzerSidecarStartCode::SessionCreateFailed => return None,
    };
    // Upstream requires `error.category !== undefined`: a model refusal that names no category is
    // not actionable, so it is not a configuration state.
    let category = error.category.clone()?;
    Some(KibitzerWakeConfiguration { category, cause, missing_providers: error.missing_providers.clone() })
}

/// The pinned recall category a sidecar child runs under when the caller names none.
pub const KIBITZER_SIDECAR_DEFAULT_CATEGORY: &str = "quick";

/// Why the sidecar's pinned category could not produce a child model (upstream
/// `KibitzerSidecarModelUnavailableCause`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerSidecarModelUnavailableCause {
    /// No registry snapshot was captured, so no honest answer exists (transient).
    RegistrySnapshotUnavailable,
    /// The pinned category's chain has no connected provider (configuration).
    CategoryUnavailable,
    /// The model resolved only OUTSIDE the pinned category, which the advisor refuses (configuration).
    BeyondCategory,
}

/// The resolver input (upstream `KibitzerSidecarModelInput`): the pinned category, the omo config and
/// the parent's registry snapshot captured synchronously at a hook (`None` means none was captured).
pub struct KibitzerSidecarModelInput<'a> {
    /// `memory.recall.category`; [`KIBITZER_SIDECAR_DEFAULT_CATEGORY`] when absent.
    pub category: Option<&'a str>,
    pub config: &'a Value,
    pub registry: Option<&'a dyn SenpiModelRegistry>,
}

/// The resolver verdict (upstream `KibitzerSidecarModelResolution`).
///
/// `Debug + Clone` only: the `Resolved` variant carries `Vec<ReflectionModelCandidate>`, which
/// derives `Clone, Debug` (no `PartialEq`), so this enum cannot derive `PartialEq`.
#[derive(Debug, Clone)]
pub enum KibitzerSidecarModelResolution {
    /// A category-sourced model: the leading model, its fallbacks (connected first, the unconnected
    /// kept reachable behind them) and the child's model-chain spec.
    Resolved {
        category: String,
        model: String,
        thinking: Option<String>,
        fallbacks: Vec<ReflectionModelCandidate>,
        chain: ChildModelChainSpec,
    },
    /// The pinned category cannot serve a model; `missing_providers` names what a `/login` would fix.
    Unavailable {
        category: String,
        cause: KibitzerSidecarModelUnavailableCause,
        missing_providers: Option<Vec<String>>,
    },
}

/// `resolveKibitzerSidecarModel`: resolves the pinned category against the live registry snapshot,
/// leads with the first CONNECTED candidate (#9216) and refuses a resolution from outside the
/// category. A `HostError` from the registry or the category resolver propagates exactly where
/// upstream would throw.
pub fn resolve_kibitzer_sidecar_model(
    input: &KibitzerSidecarModelInput<'_>,
) -> Result<KibitzerSidecarModelResolution, HostError> {
    let category = input.category.unwrap_or(KIBITZER_SIDECAR_DEFAULT_CATEGORY);
    let Some(registry) = input.registry else {
        return Ok(KibitzerSidecarModelResolution::Unavailable {
            category: category.to_string(),
            cause: KibitzerSidecarModelUnavailableCause::RegistrySnapshotUnavailable,
            missing_providers: None,
        });
    };
    let (resolved_category, model, thinking, source, fallbacks) =
        match resolve_reflection_model(category, input.config, Some(registry), None)? {
            ReflectionModelResolution::CategoryUnavailable { missing_providers, .. } => {
                return Ok(KibitzerSidecarModelResolution::Unavailable {
                    category: category.to_string(),
                    cause: KibitzerSidecarModelUnavailableCause::CategoryUnavailable,
                    missing_providers,
                });
            }
            ReflectionModelResolution::Resolved { category: resolved_category, model, thinking, source, fallbacks } => {
                (resolved_category, model, thinking, source, fallbacks)
            }
        };
    // A `source` names a resolution from OUTSIDE the pinned category, which the advisor refuses -
    // and it hides why the category itself came up empty, so the chain is asked again for the
    // providers a `/login` would revive.
    if source.is_some() {
        return Ok(KibitzerSidecarModelResolution::Unavailable {
            category: category.to_string(),
            cause: KibitzerSidecarModelUnavailableCause::BeyondCategory,
            missing_providers: chain_providers(category, input.config, registry)?,
        });
    }
    let (model, thinking, fallbacks) =
        match order_kibitzer_candidates_by_connection(&KibitzerCandidateOrderInput {
            category: &resolved_category,
            config: input.config,
            registry,
            model: &model,
            thinking: thinking.as_deref(),
            fallbacks: &fallbacks,
        })? {
            KibitzerCandidateOrder::Ordered { model, thinking, fallbacks } => (model, thinking, fallbacks),
            KibitzerCandidateOrder::NoneConnected { missing_providers } => {
                return Ok(KibitzerSidecarModelResolution::Unavailable {
                    category: resolved_category,
                    cause: KibitzerSidecarModelUnavailableCause::CategoryUnavailable,
                    missing_providers: Some(missing_providers),
                });
            }
            // The availability list was unknown: the resolution stands exactly as it was.
            KibitzerCandidateOrder::AvailabilityUnknown => (model, thinking, fallbacks),
        };
    let chain = child_model_chain_spec(ChildModelChainInput { model: &model, fallbacks: &fallbacks });
    Ok(KibitzerSidecarModelResolution::Resolved {
        category: resolved_category,
        model,
        thinking,
        fallbacks,
        chain,
    })
}

/// `chainProviders`: the category chain's unconnected providers, when the chain failed for exactly
/// that reason; `None` otherwise.
fn chain_providers(
    category: &str,
    config: &Value,
    registry: &dyn SenpiModelRegistry,
) -> Result<Option<Vec<String>>, HostError> {
    let chain = resolve_category(category, config, registry, &ResolveCategoryOptions::default())?;
    Ok(match chain {
        CategoryResolutionResult::ModelUnavailable(unavailable) => unavailable.missing_providers,
        _ => None,
    })
}

/// Everything `buildKibitzerSidecarSpec` reads (upstream `KibitzerSidecarSpecInput`).
pub struct KibitzerSidecarSpecInput {
    pub session_id: String,
    /// Child number within the session; part of the task id so a recreated child never reuses one.
    pub generation: u64,
    /// The primary agent's workspace: `read` and `grep` are scoped to it.
    pub cwd: String,
    /// `recall/sidecars/<encoded-session>/`: the child's JSONL transcript is the audit trail.
    pub session_dir: String,
    pub agent_dir: String,
    /// The parent's registry, captured synchronously at a hook; absent means no honest answer exists.
    pub model_registry: Option<HostHandle>,
    /// `ChildSpec["model"]`: the resolved model handle (`runtime.findModelReference(..)`).
    pub model: Option<HostHandle>,
    /// The model-selection projection (`selectedModel` / `fallbackModels` / `retry`).
    pub chain: ChildModelChainSpec,
    pub thinking_level: Option<String>,
    /// The persona text, read by the caller so a missing asset is reported as itself.
    pub system_prompt: String,
    /// The five member-scoped closures, in registry order.
    pub tools: Vec<ChildToolRef>,
    /// The seed (or reseed + wake) envelope: the child's first user message, verbatim.
    pub prompt: String,
}

/// `buildKibitzerSidecarSpec`: the resident child's normative `ChildSpec` - exactly the five
/// read-only closures, a bare envelope (the seed IS the first user message), and turn completion (a
/// silent turn is a finished turn). `model_registry`, `model` and `thinking_level` are spread only
/// when present, and `selected_model` / `fallback_models` / `retry` come from the resolved chain,
/// exactly as upstream's `...(x === undefined ? {} : { x })` and `...input.chain`. Every field
/// upstream does not set (`auth_storage`, `model_runtime`, `requested_model`, `resolved_model`,
/// `tool_denylist`, `agent_type`, `instructions`) stays at `ChildSpec::default()`.
pub fn build_kibitzer_sidecar_spec(input: KibitzerSidecarSpecInput) -> ChildSpec {
    ChildSpec {
        task_id: format!("kibitzer-{}-{}", input.session_id, input.generation),
        cwd: input.cwd,
        session_dir: Some(input.session_dir),
        agent_dir: Some(input.agent_dir),
        model_registry: input.model_registry,
        model: input.model,
        selected_model: input.chain.selected_model,
        fallback_models: input.chain.fallback_models,
        retry: input.chain.retry,
        thinking_level: input.thinking_level,
        tool_allowlist: Some(KIBITZER_SIDECAR_TOOL_NAMES.iter().map(|name| (*name).to_string()).collect()),
        member_scoped_tool_names: Some(input.tools.iter().map(|tool| tool.name().to_string()).collect()),
        member_scoped_tools: Some(input.tools),
        depth: 1,
        parent_session_id: input.session_id.clone(),
        root_session_id: input.session_id,
        system_prompt: Some(input.system_prompt),
        prompt_envelope: Some(ChildPromptEnvelope::Bare),
        completion: Some(ChildCompletionPolicy::Turn),
        prompt: input.prompt,
        ..ChildSpec::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_category_unavailable_with_a_category_when_classified_then_it_is_a_structured_configuration() {
        let error = KibitzerSidecarStartError::new(
            KibitzerSidecarStartCode::CategoryUnavailable,
            "no connected provider",
        )
        .with_category("quick");
        let configuration =
            kibitzer_configuration_failure(&error).expect("a category refusal is a configuration state");
        assert_eq!(configuration.cause, KibitzerWakeConfigurationCause::CategoryUnavailable);
        assert_eq!(configuration.category, "quick");
    }

    #[test]
    fn given_beyond_category_when_classified_then_the_cause_is_beyond_category() {
        let error = KibitzerSidecarStartError::new(
            KibitzerSidecarStartCode::BeyondCategory,
            "resolved outside the category",
        )
        .with_category("quick");
        let configuration =
            kibitzer_configuration_failure(&error).expect("a beyond-category refusal is a configuration state");
        assert_eq!(configuration.cause, KibitzerWakeConfigurationCause::BeyondCategory);
    }

    #[test]
    fn given_a_model_refusal_without_a_category_when_classified_then_it_is_not_a_configuration() {
        let error = KibitzerSidecarStartError::new(
            KibitzerSidecarStartCode::CategoryUnavailable,
            "no connected provider",
        );
        assert!(kibitzer_configuration_failure(&error).is_none());
    }

    #[test]
    fn given_registry_snapshot_unavailable_with_a_category_when_classified_then_it_is_not_a_configuration() {
        let error = KibitzerSidecarStartError::new(
            KibitzerSidecarStartCode::RegistrySnapshotUnavailable,
            "no registry snapshot",
        )
        .with_category("quick");
        assert!(kibitzer_configuration_failure(&error).is_none());
    }

    #[test]
    fn given_a_category_refusal_with_missing_providers_when_classified_then_the_providers_are_preserved() {
        let error = KibitzerSidecarStartError::new(
            KibitzerSidecarStartCode::CategoryUnavailable,
            "no connected provider",
        )
        .with_category("quick")
        .with_missing_providers(vec!["anthropic".to_string(), "openai".to_string()]);
        let configuration =
            kibitzer_configuration_failure(&error).expect("a category refusal is a configuration state");
        assert_eq!(
            configuration.missing_providers,
            Some(vec!["anthropic".to_string(), "openai".to_string()])
        );
    }
}
