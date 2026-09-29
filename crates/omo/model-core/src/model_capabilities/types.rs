use indexmap::IndexMap;
use serde::Deserialize;
use serde::Serialize;

use crate::model_capability_aliases::AliasSource;
use crate::provider_cache::ProviderCache;

/// `{ input?: string[]; output?: string[] }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotModalities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Vec<String>>,
}

/// Token limits. models.dev publishes integers; non-integer values are not representable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotLimit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilitiesSnapshotEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<bool>,
    #[serde(rename = "toolCall", default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<SnapshotModalities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<SnapshotLimit>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilitiesSnapshot {
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    #[serde(rename = "sourceUrl")]
    pub source_url: String,
    pub models: IndexMap<String, ModelCapabilitiesSnapshotEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolutionMode {
    SnapshotBacked,
    AliasBacked,
    HeuristicBacked,
    Unknown,
}

/// Which snapshot supplied the entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotSource {
    RuntimeSnapshot,
    BundledSnapshot,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FamilySource {
    Snapshot,
    Heuristic,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VariantsSource {
    None,
    Runtime,
    Override,
    Heuristic,
    Canonical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningEffortsSource {
    None,
    Override,
    Heuristic,
}

/// Provenance of a single capability value (superset of the per-field TS unions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilitySource {
    Runtime,
    Override,
    Heuristic,
    RuntimeSnapshot,
    BundledSnapshot,
    None,
}

impl From<SnapshotSource> for CapabilitySource {
    fn from(source: SnapshotSource) -> Self {
        match source {
            SnapshotSource::RuntimeSnapshot => Self::RuntimeSnapshot,
            SnapshotSource::BundledSnapshot => Self::BundledSnapshot,
            SnapshotSource::None => Self::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalizationDiagnostics {
    pub source: AliasSource,
    #[serde(rename = "ruleID", skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilitiesDiagnostics {
    pub resolution_mode: ResolutionMode,
    pub canonicalization: CanonicalizationDiagnostics,
    pub snapshot: SnapshotSource,
    pub family: FamilySource,
    pub variants: VariantsSource,
    pub reasoning_efforts: ReasoningEffortsSource,
    pub reasoning: CapabilitySource,
    pub supports_thinking: CapabilitySource,
    pub supports_temperature: CapabilitySource,
    pub supports_top_p: CapabilitySource,
    pub max_output_tokens: CapabilitySource,
    pub tool_call: CapabilitySource,
    pub modalities: CapabilitySource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(rename = "requestedModelID")]
    pub requested_model_id: String,
    #[serde(rename = "canonicalModelID")]
    pub canonical_model_id: String,
    pub family: Option<String>,
    pub variants: Option<Vec<String>>,
    pub reasoning_efforts: Option<Vec<String>>,
    pub reasoning: Option<bool>,
    pub supports_thinking: Option<bool>,
    pub supports_temperature: Option<bool>,
    pub supports_top_p: Option<bool>,
    pub max_output_tokens: Option<i64>,
    pub tool_call: Option<bool>,
    pub modalities: Option<SnapshotModalities>,
    pub diagnostics: ModelCapabilitiesDiagnostics,
}

/// Inputs for [`super::get_model_capabilities`]; `runtime_model` is untrusted provider JSON.
#[derive(Clone, Copy, Default)]
pub struct GetModelCapabilitiesInput<'a> {
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub runtime_model: Option<&'a serde_json::Value>,
    pub runtime_snapshot: Option<&'a ModelCapabilitiesSnapshot>,
    pub bundled_snapshot: Option<&'a ModelCapabilitiesSnapshot>,
    pub provider_cache: Option<&'a dyn ProviderCache>,
}

/// Hand-maintained capability override for a model id.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelCapabilityOverride {
    pub variants: Option<&'static [&'static str]>,
    pub reasoning_efforts: Option<&'static [&'static str]>,
    pub supports_thinking: Option<bool>,
    pub supports_temperature: Option<bool>,
    pub supports_top_p: Option<bool>,
}
