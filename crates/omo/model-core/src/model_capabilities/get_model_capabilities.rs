use std::sync::LazyLock;

use indexmap::IndexSet;
use regex::Regex;

use super::runtime_model_readers::read_runtime_model_limit_output;
use super::runtime_model_readers::read_runtime_model_modalities;
use super::runtime_model_readers::read_runtime_model_reasoning_support;
use super::runtime_model_readers::read_runtime_model_temperature_support;
use super::runtime_model_readers::read_runtime_model_thinking_support;
use super::runtime_model_readers::read_runtime_model_tool_call_support;
use super::runtime_model_readers::read_runtime_model_top_p_support;
use super::runtime_model_readers::read_runtime_model_variants;
use super::types::CanonicalizationDiagnostics;
use super::types::CapabilitySource;
use super::types::FamilySource;
use super::types::GetModelCapabilitiesInput;
use super::types::ModelCapabilities;
use super::types::ModelCapabilitiesDiagnostics;
use super::types::ModelCapabilitiesSnapshot;
use super::types::ModelCapabilitiesSnapshotEntry;
use super::types::ModelCapabilityOverride;
use super::types::ReasoningEffortsSource;
use super::types::ResolutionMode;
use super::types::SnapshotSource;
use super::types::VariantsSource;
use crate::model_capability_aliases::AliasSource;
use crate::model_capability_aliases::ModelIdAliasResolution;
use crate::model_capability_aliases::resolve_model_id_alias;
use crate::model_capability_heuristics::detect_heuristic_model_family;
use crate::model_string_parser::parse_variant_from_model_id;
use crate::provider_cache::ModelMetadata;
use crate::provider_cache::ProviderCache;
use crate::reasoning_level::SplitReasoningSuffixOptions;

/// Per-model overrides keyed by normalized model id (currently none).
const MODEL_ID_OVERRIDES: &[(&str, ModelCapabilityOverride)] = &[];

const GITHUB_COPILOT_GPT5_OVERRIDE: ModelCapabilityOverride = ModelCapabilityOverride {
    variants: Some(&["low", "medium", "high"]),
    reasoning_efforts: Some(&["none", "minimal", "low", "medium", "high"]),
    supports_thinking: None,
    supports_temperature: None,
    supports_top_p: None,
};

fn normalize_lookup_model_id(model_id: &str) -> String {
    model_id.trim().to_lowercase()
}

fn bare_model_id(model_id: &str) -> String {
    parse_variant_from_model_id(
        model_id,
        SplitReasoningSuffixOptions {
            allow_max_suffix: Some(true),
        },
    )
    .model_id
}

fn get_override(model_id: &str) -> Option<ModelCapabilityOverride> {
    let normalized = normalize_lookup_model_id(model_id);
    MODEL_ID_OVERRIDES
        .iter()
        .find(|(id, _)| *id == normalized)
        .map(|(_, capability_override)| *capability_override)
}

#[expect(clippy::expect_used, reason = "static regex literal is valid")]
fn get_provider_override(provider_id: &str, model_id: &str) -> Option<ModelCapabilityOverride> {
    static GITHUB_COPILOT_GPT5_MODEL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:^|/)gpt-5(?:[.-]|$)").expect("valid regex"));
    if provider_id.trim().to_lowercase() != "github-copilot" {
        return None;
    }
    GITHUB_COPILOT_GPT5_MODEL
        .is_match(&normalize_lookup_model_id(model_id))
        .then_some(GITHUB_COPILOT_GPT5_OVERRIDE)
}

fn strip_same_provider_prefix(provider_id: &str, model_id: &str) -> Option<String> {
    let prefix = format!("{}/", provider_id.trim());
    if prefix == "/" {
        return None;
    }
    let head = model_id.get(..prefix.len())?;
    (head.to_lowercase() == prefix.to_lowercase()).then(|| model_id[prefix.len()..].to_string())
}

fn push_unique(candidates: &mut IndexSet<String>, candidate: Option<String>) {
    if let Some(candidate) = candidate.filter(|candidate| !candidate.is_empty()) {
        candidates.insert(candidate);
    }
}

/// Full id, same-provider-prefix-stripped, variant-stripped, both; most specific first.
fn build_lookup_candidates(provider_id: &str, model_id: &str) -> IndexSet<String> {
    let bare = bare_model_id(model_id);
    let mut candidates = IndexSet::new();
    push_unique(&mut candidates, Some(model_id.to_string()));
    push_unique(
        &mut candidates,
        strip_same_provider_prefix(provider_id, model_id),
    );
    push_unique(&mut candidates, Some(bare.clone()));
    push_unique(
        &mut candidates,
        strip_same_provider_prefix(provider_id, &bare),
    );
    candidates
}

/// The provider cache matches ids exactly, so a suffixed or same-provider-prefixed request would
/// miss the advertised model. More specific forms are tried first so exact suffixed metadata wins.
fn find_provider_metadata(
    provider_cache: Option<&dyn ProviderCache>,
    provider_id: &str,
    model_id: &str,
) -> Option<ModelMetadata> {
    let provider_cache = provider_cache?;
    build_lookup_candidates(provider_id, model_id)
        .iter()
        .find_map(|candidate| provider_cache.find_provider_model_metadata(provider_id, candidate))
}

fn find_snapshot_entry<'a>(
    snapshot: Option<&'a ModelCapabilitiesSnapshot>,
    provider_id: &str,
    model_ids: &[&str],
) -> Option<&'a ModelCapabilitiesSnapshotEntry> {
    let snapshot = snapshot?;
    let normalized_provider_id = provider_id.trim();
    let mut provider_specific_candidates: Vec<String> = Vec::new();
    let mut general_candidates: Vec<String> = Vec::new();
    for model_id in model_ids {
        let unqualified_model_id = strip_same_provider_prefix(provider_id, model_id)
            .or_else(|| (!model_id.contains('/')).then(|| (*model_id).to_string()))
            .filter(|id| !id.is_empty());
        let Some(unqualified_model_id) = unqualified_model_id else {
            general_candidates.extend(build_lookup_candidates(provider_id, model_id));
            continue;
        };
        let bare = bare_model_id(&unqualified_model_id);
        if !normalized_provider_id.is_empty() {
            provider_specific_candidates
                .push(format!("{normalized_provider_id}/{unqualified_model_id}"));
            provider_specific_candidates.push(format!("{normalized_provider_id}/{bare}"));
        }
        general_candidates.push(unqualified_model_id);
        general_candidates.push(bare);
    }
    let candidates: IndexSet<String> = provider_specific_candidates
        .into_iter()
        .chain(general_candidates)
        .collect();
    candidates
        .iter()
        .find_map(|candidate| snapshot.models.get(candidate))
}

fn resolve_capability_model_alias(model_id: &str, provider_id: &str) -> ModelIdAliasResolution {
    let direct = resolve_model_id_alias(model_id, Some(provider_id));
    if direct.source != AliasSource::Canonical {
        return direct;
    }
    let bare = bare_model_id(model_id);
    if bare == model_id {
        return direct;
    }
    let bare_alias = resolve_model_id_alias(&bare, Some(provider_id));
    if bare_alias.source == AliasSource::Canonical {
        return direct;
    }
    ModelIdAliasResolution {
        requested_model_id: model_id.to_string(),
        ..bare_alias
    }
}

fn owned_strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// Resolves capabilities from runtime metadata, then runtime snapshot, then bundled snapshot,
/// then heuristic family rules, recording where each value came from.
#[must_use]
pub fn get_model_capabilities(input: GetModelCapabilitiesInput<'_>) -> ModelCapabilities {
    let canonicalization = resolve_capability_model_alias(input.model_id, input.provider_id);
    let model_override = get_override(input.model_id);
    let provider_override =
        get_provider_override(input.provider_id, &canonicalization.canonical_model_id);
    let runtime_model: Option<ModelMetadata> =
        match input.runtime_model.filter(|value| !value.is_null()) {
            Some(value) => value.as_object().cloned(),
            None => find_provider_metadata(input.provider_cache, input.provider_id, input.model_id),
        };
    let runtime_model = runtime_model.as_ref();

    let snapshot_model_ids: [&str; 2] = if canonicalization.source == AliasSource::Canonical {
        [input.model_id, &canonicalization.canonical_model_id]
    } else {
        [&canonicalization.canonical_model_id, input.model_id]
    };
    let runtime_snapshot_entry = find_snapshot_entry(
        input.runtime_snapshot,
        input.provider_id,
        &snapshot_model_ids,
    );
    let bundled_snapshot_entry = find_snapshot_entry(
        input.bundled_snapshot,
        input.provider_id,
        &snapshot_model_ids,
    );
    let snapshot_entry = runtime_snapshot_entry.or(bundled_snapshot_entry);
    let heuristic_family = detect_heuristic_model_family(&canonicalization.canonical_model_id);

    let runtime_variants = read_runtime_model_variants(runtime_model);
    let runtime_reasoning = read_runtime_model_reasoning_support(runtime_model);
    let runtime_thinking = read_runtime_model_thinking_support(runtime_model);
    let runtime_temperature = read_runtime_model_temperature_support(runtime_model);
    let runtime_top_p = read_runtime_model_top_p_support(runtime_model);
    let runtime_max_output_tokens = read_runtime_model_limit_output(runtime_model);
    let runtime_tool_call = read_runtime_model_tool_call_support(runtime_model);
    let runtime_modalities = read_runtime_model_modalities(runtime_model);

    let snapshot_family = snapshot_entry
        .and_then(|entry| entry.family.clone())
        .filter(|family| !family.is_empty());
    let snapshot_reasoning = snapshot_entry.and_then(|entry| entry.reasoning);
    let snapshot_temperature = snapshot_entry.and_then(|entry| entry.temperature);
    let snapshot_output = snapshot_entry
        .and_then(|entry| entry.limit.as_ref())
        .and_then(|limit| limit.output);
    let snapshot_tool_call = snapshot_entry.and_then(|entry| entry.tool_call);
    let snapshot_modalities = snapshot_entry.and_then(|entry| entry.modalities.clone());

    let snapshot_source = if runtime_snapshot_entry.is_some() {
        SnapshotSource::RuntimeSnapshot
    } else if bundled_snapshot_entry.is_some() {
        SnapshotSource::BundledSnapshot
    } else {
        SnapshotSource::None
    };
    let from_snapshot = CapabilitySource::from(snapshot_source);
    let family_source = if snapshot_family.is_some() {
        FamilySource::Snapshot
    } else if heuristic_family.is_some() {
        FamilySource::Heuristic
    } else {
        FamilySource::None
    };
    let override_variants = provider_override
        .and_then(|o| o.variants)
        .or_else(|| model_override.and_then(|o| o.variants));
    let override_reasoning_efforts = provider_override
        .and_then(|o| o.reasoning_efforts)
        .or_else(|| model_override.and_then(|o| o.reasoning_efforts));
    let heuristic_variants = heuristic_family.and_then(|family| family.variants);
    let heuristic_reasoning_efforts = heuristic_family.and_then(|family| family.reasoning_efforts);

    let variants_source = if runtime_variants.is_some() {
        VariantsSource::Runtime
    } else if override_variants.is_some() {
        VariantsSource::Override
    } else if heuristic_variants.is_some() {
        VariantsSource::Heuristic
    } else {
        VariantsSource::None
    };
    let reasoning_efforts_source = if override_reasoning_efforts.is_some() {
        ReasoningEffortsSource::Override
    } else if heuristic_reasoning_efforts.is_some() {
        ReasoningEffortsSource::Heuristic
    } else {
        ReasoningEffortsSource::None
    };
    let reasoning_source = match (runtime_reasoning, snapshot_reasoning) {
        (Some(_), _) => CapabilitySource::Runtime,
        (None, Some(_)) => from_snapshot,
        (None, None) => CapabilitySource::None,
    };
    let override_thinking = model_override.and_then(|o| o.supports_thinking);
    let heuristic_thinking = heuristic_family.and_then(|family| family.supports_thinking);
    let supports_thinking_source = if override_thinking.is_some() {
        CapabilitySource::Override
    } else if runtime_thinking.is_some() {
        CapabilitySource::Runtime
    } else if snapshot_reasoning.is_some() {
        from_snapshot
    } else if heuristic_thinking.is_some() {
        CapabilitySource::Heuristic
    } else {
        CapabilitySource::None
    };
    let override_temperature = model_override.and_then(|o| o.supports_temperature);
    let supports_temperature_source = if runtime_temperature.is_some() {
        CapabilitySource::Runtime
    } else if override_temperature.is_some() {
        CapabilitySource::Override
    } else if snapshot_temperature.is_some() {
        from_snapshot
    } else {
        CapabilitySource::None
    };
    let override_top_p = model_override.and_then(|o| o.supports_top_p);
    let supports_top_p_source = if runtime_top_p.is_some() {
        CapabilitySource::Runtime
    } else if override_top_p.is_some() {
        CapabilitySource::Override
    } else {
        CapabilitySource::None
    };
    let pick = |runtime_present: bool, snapshot_present: bool| {
        if runtime_present {
            CapabilitySource::Runtime
        } else if snapshot_present {
            from_snapshot
        } else {
            CapabilitySource::None
        }
    };
    let max_output_tokens_source = pick(
        runtime_max_output_tokens.is_some(),
        snapshot_output.is_some(),
    );
    let tool_call_source = pick(runtime_tool_call.is_some(), snapshot_tool_call.is_some());
    let modalities_source = pick(runtime_modalities.is_some(), snapshot_modalities.is_some());

    let resolution_mode = match canonicalization.source {
        AliasSource::Canonical if snapshot_source != SnapshotSource::None => {
            ResolutionMode::SnapshotBacked
        }
        AliasSource::Canonical
            if family_source == FamilySource::Heuristic
                || variants_source == VariantsSource::Heuristic
                || reasoning_efforts_source == ReasoningEffortsSource::Heuristic =>
        {
            ResolutionMode::HeuristicBacked
        }
        AliasSource::Canonical => ResolutionMode::Unknown,
        AliasSource::ExactAlias | AliasSource::PatternAlias => ResolutionMode::AliasBacked,
    };

    ModelCapabilities {
        family: snapshot_entry
            .and_then(|entry| entry.family.clone())
            .or_else(|| heuristic_family.map(|family| family.family.to_string())),
        variants: runtime_variants
            .or_else(|| override_variants.or(heuristic_variants).map(owned_strings)),
        reasoning_efforts: override_reasoning_efforts
            .or(heuristic_reasoning_efforts)
            .map(owned_strings),
        reasoning: runtime_reasoning.or(snapshot_reasoning),
        supports_thinking: override_thinking
            .or(runtime_thinking)
            .or(snapshot_reasoning)
            .or(heuristic_thinking),
        supports_temperature: runtime_temperature
            .or(override_temperature)
            .or(snapshot_temperature),
        supports_top_p: runtime_top_p.or(override_top_p),
        max_output_tokens: runtime_max_output_tokens.or(snapshot_output),
        tool_call: runtime_tool_call.or(snapshot_tool_call),
        modalities: runtime_modalities.or(snapshot_modalities),
        diagnostics: ModelCapabilitiesDiagnostics {
            resolution_mode,
            canonicalization: CanonicalizationDiagnostics {
                source: canonicalization.source,
                rule_id: canonicalization.rule_id.clone(),
            },
            snapshot: snapshot_source,
            family: family_source,
            variants: variants_source,
            reasoning_efforts: reasoning_efforts_source,
            reasoning: reasoning_source,
            supports_thinking: supports_thinking_source,
            supports_temperature: supports_temperature_source,
            supports_top_p: supports_top_p_source,
            max_output_tokens: max_output_tokens_source,
            tool_call: tool_call_source,
            modalities: modalities_source,
        },
        requested_model_id: canonicalization.requested_model_id,
        canonical_model_id: canonicalization.canonical_model_id,
    }
}
