use std::sync::LazyLock;

use regex::Regex;

use crate::reasoning_level::SplitReasoningSuffixOptions;
use crate::reasoning_level::split_reasoning_suffix;

/// `{ modelID; variant? }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedVariant {
    pub model_id: String,
    pub variant: Option<String>,
}

/// `{ providerID; modelID; variant? }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModelString {
    pub provider_id: String,
    pub model_id: String,
    pub variant: Option<String>,
}

#[expect(clippy::expect_used, reason = "static regex literals are valid")]
pub(crate) static PARENTHESIZED_VARIANT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.*)\(([^()]+)\)\s*$").expect("valid regex"));
#[expect(clippy::expect_used, reason = "static regex literals are valid")]
pub(crate) static SPACE_VARIANT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(.*\S)\s+([a-z][a-z0-9_-]*)$").expect("valid regex"));

/// Extracts `(variant)`, `:level`, or ` variant` suffix syntax from a model id.
#[must_use]
pub fn parse_variant_from_model_id(
    raw_model_id: &str,
    options: SplitReasoningSuffixOptions,
) -> ParsedVariant {
    let trimmed_model_id = raw_model_id.trim();
    if trimmed_model_id.is_empty() {
        return ParsedVariant {
            model_id: String::new(),
            variant: None,
        };
    }

    if let Some(captures) = PARENTHESIZED_VARIANT.captures(trimmed_model_id) {
        let model_id = captures
            .get(1)
            .map_or("", |m| m.as_str())
            .trim()
            .to_string();
        let variant = captures
            .get(2)
            .map(|m| m.as_str().trim().to_string())
            .filter(|variant| !variant.is_empty());
        return ParsedVariant { model_id, variant };
    }

    let suffixed_model = split_reasoning_suffix(trimmed_model_id, options);
    if suffixed_model.level.is_some() {
        return ParsedVariant {
            model_id: suffixed_model.base,
            variant: suffixed_model.level,
        };
    }

    if let Some(captures) = SPACE_VARIANT.captures(trimmed_model_id) {
        let model_id = captures
            .get(1)
            .map_or("", |m| m.as_str())
            .trim()
            .to_string();
        let variant = captures
            .get(2)
            .map_or(String::new(), |m| m.as_str().trim().to_lowercase());
        if !variant.is_empty() {
            return ParsedVariant {
                model_id,
                variant: Some(variant),
            };
        }
    }

    ParsedVariant {
        model_id: trimmed_model_id.to_string(),
        variant: None,
    }
}

/// Parses `provider/model[variant syntax]`; `None` when either half is missing.
#[must_use]
pub fn parse_model_string(model: &str) -> Option<ParsedModelString> {
    let (provider_id, raw_model_id) = model.trim().split_once('/')?;
    let provider_id = provider_id.trim();
    let raw_model_id = raw_model_id.trim();
    if provider_id.is_empty() || raw_model_id.is_empty() {
        return None;
    }

    let parsed_model = parse_variant_from_model_id(
        raw_model_id,
        SplitReasoningSuffixOptions {
            allow_max_suffix: Some(true),
        },
    );
    if parsed_model.model_id.is_empty() {
        return None;
    }

    Some(ParsedModelString {
        provider_id: provider_id.to_string(),
        model_id: parsed_model.model_id,
        variant: parsed_model.variant,
    })
}
