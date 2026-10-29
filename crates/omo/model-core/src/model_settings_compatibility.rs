use serde_json::Value;

use crate::js_value::json_stringify;
use crate::model_capability_heuristics::detect_heuristic_model_family;
use crate::model_family_detectors::is_claude_opus47_or_later_model;
use crate::reasoning_level::clamp_reasoning_level;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibilityField {
    Variant,
    ReasoningEffort,
    Temperature,
    TopP,
    MaxTokens,
    Thinking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibilityChangeReason {
    UnsupportedByModelFamily,
    UnknownModelFamily,
    UnsupportedByModelMetadata,
    MaxOutputLimit,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DesiredModelSettings {
    pub variant: Option<String>,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<f64>,
    pub thinking: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CompatibilityCapabilities {
    pub variants: Option<Vec<String>>,
    pub reasoning_efforts: Option<Vec<String>>,
    pub supports_temperature: Option<bool>,
    pub supports_top_p: Option<bool>,
    pub max_output_tokens: Option<f64>,
    pub supports_thinking: Option<bool>,
}

impl From<&crate::model_capabilities::ModelCapabilities> for CompatibilityCapabilities {
    fn from(capabilities: &crate::model_capabilities::ModelCapabilities) -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "token limits are far below 2^53"
        )]
        let max_output_tokens = capabilities.max_output_tokens.map(|tokens| tokens as f64);
        Self {
            variants: capabilities.variants.clone(),
            reasoning_efforts: capabilities.reasoning_efforts.clone(),
            supports_temperature: capabilities.supports_temperature,
            supports_top_p: capabilities.supports_top_p,
            max_output_tokens,
            supports_thinking: capabilities.supports_thinking,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelSettingsCompatibilityInput {
    pub provider_id: String,
    pub model_id: String,
    pub desired: DesiredModelSettings,
    pub capabilities: Option<CompatibilityCapabilities>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSettingsCompatibilityChange {
    pub field: CompatibilityField,
    pub from: String,
    pub to: Option<String>,
    pub reason: CompatibilityChangeReason,
}

/// Outer `Option` on numeric/thinking fields = key presence; inner = the (possibly dropped) value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelSettingsCompatibilityResult {
    pub variant: Option<String>,
    pub reasoning_effort: Option<String>,
    pub temperature: Option<Option<f64>>,
    pub top_p: Option<Option<f64>>,
    pub max_tokens: Option<Option<f64>>,
    pub thinking: Option<Option<Value>>,
    pub changes: Vec<ModelSettingsCompatibilityChange>,
}

fn non_empty_lowercased(values: Option<&Vec<String>>) -> Option<Vec<String>> {
    values
        .filter(|values| !values.is_empty())
        .map(|values| values.iter().map(|value| value.to_lowercase()).collect())
}

struct FieldResolution {
    value: Option<String>,
    reason: Option<CompatibilityChangeReason>,
}

fn owned(values: Option<&[&str]>) -> Option<Vec<String>> {
    values.map(|values| values.iter().map(|value| (*value).to_string()).collect())
}

fn resolve_field(
    normalized: &str,
    family_caps: Option<&[String]>,
    family_known: bool,
    metadata_override: Option<&[String]>,
    family_alias: Option<&str>,
) -> FieldResolution {
    let contains = |values: Option<&[String]>, value: &str| {
        values.is_some_and(|values| values.iter().any(|v| v == value))
    };
    if let Some(aliased) = family_alias.filter(|aliased| !aliased.is_empty())
        && (contains(metadata_override, aliased) || contains(family_caps, aliased))
    {
        return FieldResolution {
            value: Some(aliased.to_string()),
            reason: Some(CompatibilityChangeReason::UnsupportedByModelFamily),
        };
    }

    let downgrade = |allowed: &[String], reason| {
        if allowed.iter().any(|value| value == normalized) {
            FieldResolution {
                value: Some(normalized.to_string()),
                reason: None,
            }
        } else {
            FieldResolution {
                value: clamp_reasoning_level(normalized, allowed),
                reason: Some(reason),
            }
        }
    };
    if let Some(metadata_override) = metadata_override {
        return downgrade(
            metadata_override,
            CompatibilityChangeReason::UnsupportedByModelMetadata,
        );
    }
    if let Some(family_caps) = family_caps {
        return downgrade(
            family_caps,
            CompatibilityChangeReason::UnsupportedByModelFamily,
        );
    }
    FieldResolution {
        value: None,
        reason: Some(if family_known {
            CompatibilityChangeReason::UnsupportedByModelFamily
        } else {
            CompatibilityChangeReason::UnknownModelFamily
        }),
    }
}

/// JavaScript `String(number)` for the values settings carry.
fn js_number(value: f64) -> String {
    serde_json::Number::from_f64(value).map_or_else(
        || value.to_string(),
        |number| {
            let text = number.to_string();
            text.strip_suffix(".0").map_or(text.clone(), str::to_string)
        },
    )
}

/// Clamps variant, reasoning effort, temperature, topP, maxTokens, and thinking to what the model
/// family and capability metadata support, recording every change.
#[must_use]
pub fn resolve_compatible_model_settings(
    input: &ModelSettingsCompatibilityInput,
) -> ModelSettingsCompatibilityResult {
    let family = detect_heuristic_model_family(&input.model_id);
    let family_known = family.is_some();
    let capabilities = input.capabilities.as_ref();
    let mut changes = Vec::new();
    let metadata_variants = non_empty_lowercased(capabilities.and_then(|c| c.variants.as_ref()));
    let metadata_reasoning_efforts =
        non_empty_lowercased(capabilities.and_then(|c| c.reasoning_efforts.as_ref()));
    let family_variants = owned(family.and_then(|f| f.variants));
    let family_reasoning_efforts = owned(family.and_then(|f| f.reasoning_efforts));

    let variant = input.desired.variant.as_ref().and_then(|variant| {
        let normalized = variant.to_lowercase();
        let resolved = resolve_field(
            &normalized,
            family_variants.as_deref(),
            family_known,
            metadata_variants.as_deref(),
            None,
        );
        if let Some(reason) = resolved
            .reason
            .filter(|_| resolved.value.as_deref() != Some(normalized.as_str()))
        {
            changes.push(ModelSettingsCompatibilityChange {
                field: CompatibilityField::Variant,
                from: variant.clone(),
                to: resolved.value.clone(),
                reason,
            });
        }
        resolved.value
    });

    let reasoning_effort = input.desired.reasoning_effort.as_ref().and_then(|effort| {
        let normalized = effort.to_lowercase();
        let resolved = resolve_field(
            &normalized,
            family_reasoning_efforts.as_deref(),
            family_known,
            metadata_reasoning_efforts.as_deref(),
            family.and_then(|f| f.reasoning_effort_alias(&normalized)),
        );
        if let Some(reason) = resolved
            .reason
            .filter(|_| resolved.value.as_deref() != Some(normalized.as_str()))
        {
            changes.push(ModelSettingsCompatibilityChange {
                field: CompatibilityField::ReasoningEffort,
                from: effort.clone(),
                to: resolved.value.clone(),
                reason,
            });
        }
        resolved.value
    });

    let metadata_supports_temperature = capabilities.and_then(|c| c.supports_temperature);
    let family_disallows_temperature = metadata_supports_temperature.is_none()
        && (is_claude_opus47_or_later_model(&input.model_id)
            || family.and_then(|f| f.supports_temperature) == Some(false));
    let temperature = input.desired.temperature.map(|temperature| {
        if metadata_supports_temperature == Some(false) || family_disallows_temperature {
            changes.push(ModelSettingsCompatibilityChange {
                field: CompatibilityField::Temperature,
                from: js_number(temperature),
                to: None,
                reason: if metadata_supports_temperature == Some(false) {
                    CompatibilityChangeReason::UnsupportedByModelMetadata
                } else {
                    CompatibilityChangeReason::UnsupportedByModelFamily
                },
            });
            None
        } else {
            Some(temperature)
        }
    });

    let top_p = input.desired.top_p.map(|top_p| {
        if capabilities.and_then(|c| c.supports_top_p) == Some(false) {
            changes.push(ModelSettingsCompatibilityChange {
                field: CompatibilityField::TopP,
                from: js_number(top_p),
                to: None,
                reason: CompatibilityChangeReason::UnsupportedByModelMetadata,
            });
            None
        } else {
            Some(top_p)
        }
    });

    let max_tokens = input.desired.max_tokens.map(|max_tokens| {
        if max_tokens <= 0.0 {
            return None;
        }
        match capabilities.and_then(|c| c.max_output_tokens) {
            Some(limit) if limit > 0.0 && max_tokens > limit => {
                changes.push(ModelSettingsCompatibilityChange {
                    field: CompatibilityField::MaxTokens,
                    from: js_number(max_tokens),
                    to: Some(js_number(limit)),
                    reason: CompatibilityChangeReason::MaxOutputLimit,
                });
                Some(limit)
            }
            _ => Some(max_tokens),
        }
    });

    let thinking = input.desired.thinking.as_ref().map(|thinking| {
        if capabilities.and_then(|c| c.supports_thinking) == Some(false) {
            changes.push(ModelSettingsCompatibilityChange {
                field: CompatibilityField::Thinking,
                from: json_stringify(thinking),
                to: None,
                reason: CompatibilityChangeReason::UnsupportedByModelMetadata,
            });
            None
        } else {
            Some(thinking.clone())
        }
    });

    ModelSettingsCompatibilityResult {
        variant,
        reasoning_effort,
        temperature,
        top_p,
        max_tokens,
        thinking,
        changes,
    }
}
