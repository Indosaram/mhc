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

fn serialize_source<S: serde::Serializer, T: Serialize>(
    source: &T,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct Source<'a, T> {
        source: &'a T,
    }
    Source { source }.serialize(serializer)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilitiesDiagnostics {
    pub resolution_mode: ResolutionMode,
    pub canonicalization: CanonicalizationDiagnostics,
    #[serde(serialize_with = "serialize_source")]
    pub snapshot: SnapshotSource,
    #[serde(serialize_with = "serialize_source")]
    pub family: FamilySource,
    #[serde(serialize_with = "serialize_source")]
    pub variants: VariantsSource,
    #[serde(serialize_with = "serialize_source")]
    pub reasoning_efforts: ReasoningEffortsSource,
    #[serde(serialize_with = "serialize_source")]
    pub reasoning: CapabilitySource,
    #[serde(serialize_with = "serialize_source")]
    pub supports_thinking: CapabilitySource,
    #[serde(serialize_with = "serialize_source")]
    pub supports_temperature: CapabilitySource,
    #[serde(serialize_with = "serialize_source")]
    pub supports_top_p: CapabilitySource,
    #[serde(serialize_with = "serialize_source")]
    pub max_output_tokens: CapabilitySource,
    #[serde(serialize_with = "serialize_source")]
    pub tool_call: CapabilitySource,
    #[serde(serialize_with = "serialize_source")]
    pub modalities: CapabilitySource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(rename = "requestedModelID")]
    pub requested_model_id: String,
    #[serde(rename = "canonicalModelID")]
    pub canonical_model_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variants: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_efforts: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_temperature: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_top_p: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_capabilities_omit_unset_values_and_wrap_diagnostic_sources() {
        let result = crate::get_model_capabilities(GetModelCapabilitiesInput {
            provider_id: "custom", model_id: "unknown", ..Default::default()
        });
        let serialized = serde_json::to_value(result).expect("capabilities serialize");
        assert_eq!(serialized, json!({
            "requestedModelID": "unknown",
            "canonicalModelID": "unknown",
            "diagnostics": {
                "resolutionMode": "unknown",
                "canonicalization": {"source": "canonical"},
                "snapshot": {"source": "none"},
                "family": {"source": "none"},
                "variants": {"source": "none"},
                "reasoningEfforts": {"source": "none"},
                "reasoning": {"source": "none"},
                "supportsThinking": {"source": "none"},
                "supportsTemperature": {"source": "none"},
                "supportsTopP": {"source": "none"},
                "maxOutputTokens": {"source": "none"},
                "toolCall": {"source": "none"},
                "modalities": {"source": "none"}
            }
        }));
    }

    #[test]
    fn serialization_retains_false_zero_and_empty_capabilities() {
        let runtime = json!({"reasoning": false, "temperature": false, "topP": false,
            "toolCall": false});
        let mut result = crate::get_model_capabilities(GetModelCapabilitiesInput {
            provider_id: "custom", model_id: "unknown", runtime_model: Some(&runtime), ..Default::default()
        });
        result.max_output_tokens = Some(0);
        result.variants = Some(Vec::new());
        let serialized = serde_json::to_value(result).expect("capabilities serialize");
        assert_eq!(serialized["reasoning"], false);
        assert_eq!(serialized["supportsThinking"], false);
        assert_eq!(serialized["supportsTemperature"], false);
        assert_eq!(serialized["supportsTopP"], false);
        assert_eq!(serialized["toolCall"], false);
        assert_eq!(serialized["maxOutputTokens"], 0);
        assert_eq!(serialized["variants"], json!([]));
        assert_eq!(serialized["diagnostics"]["reasoning"], json!({"source": "runtime"}));
    }
}
