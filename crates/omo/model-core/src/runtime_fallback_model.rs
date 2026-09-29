use serde_json::Value;

use crate::model_string_parser::parse_model_string;

/// Stringifies a runtime model: strings pass through; `{ providerID, modelID, variant? }` records
/// become `provider/model` or `provider/model(variant)`.
#[must_use]
pub fn stringify_runtime_fallback_model(model: &Value) -> Option<String> {
    if let Value::String(model) = model {
        return Some(model.clone());
    }
    let record = model.as_object()?;
    let provider_id = record.get("providerID")?.as_str()?.trim();
    let trimmed_model_id = record.get("modelID")?.as_str()?.trim();
    if provider_id.is_empty() || trimmed_model_id.is_empty() {
        return None;
    }
    let base_model = format!("{provider_id}/{trimmed_model_id}");
    match record
        .get("variant")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(variant) => Some(format!("{base_model}({variant})")),
        None => Some(base_model),
    }
}

/// Like [`stringify_runtime_fallback_model`] but applies `variant` when the model has none.
#[must_use]
pub fn stringify_runtime_fallback_model_with_variant(
    model: &Value,
    variant: Option<&Value>,
) -> Option<String> {
    let base_model = stringify_runtime_fallback_model(model)?;
    let Some(fallback_variant) = variant
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Some(base_model);
    };
    match parse_model_string(&base_model) {
        Some(parsed) if parsed.variant.is_none() => Some(format!(
            "{}/{}({fallback_variant})",
            parsed.provider_id, parsed.model_id
        )),
        _ => Some(base_model),
    }
}

fn is_claude_family(model_id: &str) -> bool {
    ["claude-opus-", "claude-sonnet-", "claude-haiku-"]
        .iter()
        .any(|prefix| model_id.starts_with(prefix))
}

fn canonicalize_runtime_fallback_model_id(model_id: &str) -> String {
    let dotted_model_id = model_id.to_lowercase().replace('.', "-");
    if !is_claude_family(&dotted_model_id) {
        return dotted_model_id;
    }
    let mut canonical = dotted_model_id.as_str();
    for suffix in ["-thinking", "-max", "-high"] {
        canonical = canonical.strip_suffix(suffix).unwrap_or(canonical);
    }
    canonical.to_string()
}

fn parse_canonical_runtime_fallback_model(model: &str) -> Option<(String, String)> {
    let parsed = parse_model_string(model)?;
    let canonical_model_id = canonicalize_runtime_fallback_model_id(&parsed.model_id);
    let provider_id = if is_claude_family(&canonical_model_id) {
        "anthropic-compatible-claude".to_string()
    } else {
        parsed.provider_id.to_lowercase()
    };
    let model_id = match parsed.variant {
        Some(variant) => format!("{canonical_model_id}::{}", variant.to_lowercase()),
        None => canonical_model_id,
    };
    Some((provider_id, model_id))
}

/// Equivalence up to case, dotted versions, Claude provider family, and Claude tier suffixes.
#[must_use]
pub fn are_runtime_fallback_models_equivalent(
    candidate: Option<&str>,
    current: Option<&str>,
) -> bool {
    let (Some(candidate), Some(current)) = (
        candidate.filter(|c| !c.is_empty()),
        current.filter(|c| !c.is_empty()),
    ) else {
        return false;
    };
    match (
        parse_canonical_runtime_fallback_model(candidate),
        parse_canonical_runtime_fallback_model(current),
    ) {
        (Some(parsed_candidate), Some(parsed_current)) => parsed_candidate == parsed_current,
        _ => candidate.to_lowercase() == current.to_lowercase(),
    }
}
