//! Port of senpi packages/coding-agent/src/core/compaction-settings-access.ts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::compaction::ideal_settings::IdealCompactionSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionModelOverride {
    pub reserve_tokens: Option<i64>,
    pub keep_recent_tokens: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionModelSelector<'a> {
    pub provider: &'a str,
    pub id: &'a str,
}

/// senpi's CompactionSettings extends IdealCompactionSettings, so the JSON is flat; the ideal
/// fields are inlined here and projected back through ideal().
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    pub grace_band_enabled: Option<bool>,
    pub tool_admission_enabled: Option<bool>,
    pub reminder_enabled: Option<bool>,
    pub reserve_scaling_enabled: Option<bool>,
    pub speculative_lead_tokens: Option<f64>,
    pub enabled: Option<bool>,
    pub reserve_tokens: Option<i64>,
    pub keep_recent_tokens: Option<i64>,
    pub speculative_enabled: Option<bool>,
    pub speculative_fraction: Option<f64>,
    pub speculative_cooldown_ms: Option<f64>,
    pub restoration_enabled: Option<bool>,
    pub restoration_max_items: Option<f64>,
    pub restoration_max_tokens_per_item: Option<f64>,
    pub restoration_max_total_tokens: Option<f64>,
    pub restoration_context_ratio: Option<f64>,
    pub idle_compaction_enabled: Option<bool>,
    pub summarization_max_duration_ms: Option<f64>,
    pub model_overrides: Option<BTreeMap<String, CompactionModelOverride>>,
}

/// senpi `isNonNegativeSafeInteger` read of one configured token budget: absent is `None`, any
/// other value must be a non-negative safe integer (JSON `16384.0` is one), else the error names it.
fn token_budget(value: Option<&Value>, name: &str) -> Result<Option<i64>, String> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    let Some(value) = value else {
        return Ok(None);
    };
    match value.as_f64() {
        Some(number) if number.fract() == 0.0 && number.abs() <= MAX_SAFE_INTEGER => {
            Ok(Some(number as i64))
        }
        _ => Err(format!(
            "Invalid {name} setting: {value}. Expected a non-negative safe integer."
        )),
    }
}

impl<'de> Deserialize<'de> for CompactionModelOverride {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let Some(object) = value.as_object() else {
            return Err(serde::de::Error::custom(format!(
                "Invalid compaction.modelOverrides setting: {value}. Expected an object."
            )));
        };
        Ok(Self {
            reserve_tokens: token_budget(object.get("reserveTokens"), "compaction.modelOverrides reserveTokens")
                .map_err(serde::de::Error::custom)?,
            keep_recent_tokens: token_budget(
                object.get("keepRecentTokens"),
                "compaction.modelOverrides keepRecentTokens",
            )
            .map_err(serde::de::Error::custom)?,
        })
    }
}

/// senpi `resolveCompactionSettings` reads every field with a `typeof` check and falls back to
/// its default, so one mistyped setting never disables compaction; only the token budgets reject
/// invalid configuration.
impl<'de> Deserialize<'de> for CompactionSettings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let empty = serde_json::Map::new();
        let object = value.as_object().unwrap_or(&empty);
        let flag = |key: &str| object.get(key).and_then(Value::as_bool);
        let number = |key: &str| object.get(key).and_then(Value::as_f64).filter(|number| number.is_finite());
        let token = |key: &str| {
            token_budget(object.get(key), &format!("compaction.{key}")).map_err(serde::de::Error::custom)
        };
        let model_overrides = match object.get("modelOverrides") {
            Some(Value::Object(overrides)) => Some(
                overrides
                    .iter()
                    .map(|(key, entry)| {
                        CompactionModelOverride::deserialize(entry)
                            .map(|entry| (key.clone(), entry))
                            .map_err(|error| serde::de::Error::custom(error.to_string()))
                    })
                    .collect::<Result<BTreeMap<_, _>, D::Error>>()?,
            ),
            _ => None,
        };
        Ok(Self {
            grace_band_enabled: flag("graceBandEnabled"),
            tool_admission_enabled: flag("toolAdmissionEnabled"),
            reminder_enabled: flag("reminderEnabled"),
            reserve_scaling_enabled: flag("reserveScalingEnabled"),
            speculative_lead_tokens: number("speculativeLeadTokens"),
            enabled: flag("enabled"),
            reserve_tokens: token("reserveTokens")?,
            keep_recent_tokens: token("keepRecentTokens")?,
            speculative_enabled: flag("speculativeEnabled"),
            speculative_fraction: number("speculativeFraction"),
            speculative_cooldown_ms: number("speculativeCooldownMs"),
            restoration_enabled: flag("restorationEnabled"),
            restoration_max_items: number("restorationMaxItems"),
            restoration_max_tokens_per_item: number("restorationMaxTokensPerItem"),
            restoration_max_total_tokens: number("restorationMaxTotalTokens"),
            restoration_context_ratio: number("restorationContextRatio"),
            idle_compaction_enabled: flag("idleCompactionEnabled"),
            summarization_max_duration_ms: number("summarizationMaxDurationMs"),
            model_overrides,
        })
    }
}

impl CompactionSettings {
    pub fn ideal(&self) -> IdealCompactionSettings {
        IdealCompactionSettings {
            grace_band_enabled: self.grace_band_enabled,
            tool_admission_enabled: self.tool_admission_enabled,
            reminder_enabled: self.reminder_enabled,
            reserve_scaling_enabled: self.reserve_scaling_enabled,
            speculative_lead_tokens: self.speculative_lead_tokens,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTokenField {
    ReserveTokens,
    KeepRecentTokens,
}

impl CompactionTokenField {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReserveTokens => "reserveTokens",
            Self::KeepRecentTokens => "keepRecentTokens",
        }
    }
}

fn default_token_setting(field: CompactionTokenField) -> i64 {
    match field {
        CompactionTokenField::ReserveTokens => 16384,
        CompactionTokenField::KeepRecentTokens => 20000,
    }
}

pub fn compaction_enabled(settings: Option<&CompactionSettings>) -> bool {
    settings.and_then(|settings| settings.enabled).unwrap_or(true)
}

/// Resolves one token budget through the model override, then the ordinary setting, then the
/// built-in default. A configured-but-invalid value is an error rather than a silent fallback.
pub fn compaction_token_setting(
    settings: Option<&CompactionSettings>,
    field: CompactionTokenField,
    for_model: Option<CompactionModelSelector<'_>>,
) -> Result<i64, String> {
    let ordinary = settings.and_then(|settings| match field {
        CompactionTokenField::ReserveTokens => settings.reserve_tokens,
        CompactionTokenField::KeepRecentTokens => settings.keep_recent_tokens,
    });
    if let Some(value) = ordinary
        && value < 0
    {
        return Err(format!(
            "Invalid compaction.{} setting: {value}. Expected a non-negative safe integer.",
            field.as_str()
        ));
    }

    let model_key = for_model.map(|selector| format!("{}/{}", selector.provider, selector.id));
    let override_value = match (&model_key, settings.and_then(|settings| settings.model_overrides.as_ref())) {
        (Some(key), Some(overrides)) => overrides.get(key).and_then(|entry| match field {
            CompactionTokenField::ReserveTokens => entry.reserve_tokens,
            CompactionTokenField::KeepRecentTokens => entry.keep_recent_tokens,
        }),
        _ => None,
    };
    if let Some(value) = override_value
        && value < 0
    {
        let key = model_key.unwrap_or_default();
        return Err(format!(
            "Invalid compaction.modelOverrides[\"{key}\"].{} setting: {value}. Expected a non-negative safe integer.",
            field.as_str()
        ));
    }
    Ok(override_value.or(ordinary).unwrap_or_else(|| default_token_setting(field)))
}

pub fn compaction_reserve_tokens(
    settings: Option<&CompactionSettings>,
    for_model: Option<CompactionModelSelector<'_>>,
) -> Result<i64, String> {
    compaction_token_setting(settings, CompactionTokenField::ReserveTokens, for_model)
}

pub fn compaction_keep_recent_tokens(
    settings: Option<&CompactionSettings>,
    for_model: Option<CompactionModelSelector<'_>>,
) -> Result<i64, String> {
    compaction_token_setting(settings, CompactionTokenField::KeepRecentTokens, for_model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selector<'a>() -> CompactionModelSelector<'a> {
        CompactionModelSelector { provider: "anthropic", id: "claude" }
    }

    #[test]
    fn compaction_defaults_to_enabled() {
        assert!(compaction_enabled(None));
        let settings = CompactionSettings { enabled: Some(false), ..Default::default() };
        assert!(!compaction_enabled(Some(&settings)));
    }

    #[test]
    fn token_budgets_fall_back_to_the_built_in_defaults() {
        assert_eq!(compaction_reserve_tokens(None, None).expect("reserve"), 16384);
        assert_eq!(compaction_keep_recent_tokens(None, None).expect("keep"), 20000);
    }

    #[test]
    fn an_ordinary_setting_overrides_the_default() {
        let settings = CompactionSettings { reserve_tokens: Some(4096), ..Default::default() };
        assert_eq!(compaction_reserve_tokens(Some(&settings), None).expect("reserve"), 4096);
    }

    #[test]
    fn a_model_override_wins_over_the_ordinary_setting() {
        let mut overrides = BTreeMap::new();
        overrides.insert("anthropic/claude".to_owned(), CompactionModelOverride { reserve_tokens: Some(2048), keep_recent_tokens: None });
        let settings = CompactionSettings { reserve_tokens: Some(4096), model_overrides: Some(overrides), ..Default::default() };
        assert_eq!(compaction_reserve_tokens(Some(&settings), Some(selector())).expect("reserve"), 2048);
    }

    #[test]
    fn an_invalid_ordinary_setting_is_an_error() {
        let settings = CompactionSettings { reserve_tokens: Some(-1), ..Default::default() };
        assert!(compaction_reserve_tokens(Some(&settings), None).is_err());
    }

    #[test]
    fn an_invalid_override_is_an_error_naming_the_key() {
        let mut overrides = BTreeMap::new();
        overrides.insert("anthropic/claude".to_owned(), CompactionModelOverride { reserve_tokens: Some(-5), keep_recent_tokens: None });
        let settings = CompactionSettings { model_overrides: Some(overrides), ..Default::default() };
        let error = compaction_reserve_tokens(Some(&settings), Some(selector())).expect_err("error");
        assert!(error.contains("anthropic/claude"));
    }

    #[test]
    fn the_ideal_projection_carries_the_ideal_fields() {
        let settings = CompactionSettings { grace_band_enabled: Some(false), reserve_scaling_enabled: Some(true), ..Default::default() };
        let ideal = settings.ideal();
        assert_eq!(ideal.grace_band_enabled, Some(false));
        assert_eq!(ideal.reserve_scaling_enabled, Some(true));
    }

    #[test]
    fn settings_deserialize_from_flat_camel_case_json() {
        let settings: CompactionSettings =
            serde_json::from_str(r#"{"enabled":true,"reserveTokens":4096,"graceBandEnabled":false,"modelOverrides":{"a/b":{"reserveTokens":1}}}"#)
                .expect("settings");
        assert_eq!(settings.reserve_tokens, Some(4096));
        assert_eq!(settings.grace_band_enabled, Some(false));
        assert_eq!(settings.model_overrides.as_ref().and_then(|m| m.get("a/b")).and_then(|o| o.reserve_tokens), Some(1));
    }

    #[test]
    fn a_mistyped_setting_falls_back_while_token_budgets_stay_strict() {
        let settings: CompactionSettings =
            serde_json::from_str(r#"{"enabled":"false","speculativeFraction":"0.5","reserveTokens":4096.0}"#)
                .expect("settings");
        assert_eq!(settings.enabled, None);
        assert_eq!(settings.speculative_fraction, None);
        assert_eq!(settings.reserve_tokens, Some(4096));
        assert!(serde_json::from_str::<CompactionSettings>(r#"{"keepRecentTokens":"1"}"#).is_err());
        assert!(serde_json::from_str::<CompactionSettings>(r#"{"keepRecentTokens":1.5}"#).is_err());
    }
}
