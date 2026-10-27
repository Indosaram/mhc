//! Model strings, reasoning levels and the read-time folding of legacy model fields (spec §4.5).

use serde_json::{Map, Value};

/// Known reasoning levels, in increasing order, followed by `auto`.
pub const REASONING_LEVELS: [&str; 8] = [
    "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto",
];

/// Legacy model-object fields folded into the canonical ones on read.
pub const LEGACY_MODEL_FIELDS: [&str; 5] = [
    "variant",
    "reasoningEffort",
    "thinking",
    "textVerbosity",
    "providerOptions",
];

/// A reasoning token as a known level: trimmed, lower-cased, `none` read as `off`.
pub fn known_level(token: &str) -> Option<&'static str> {
    let lower = token.trim().to_ascii_lowercase();
    let lower = if lower == "none" {
        "off".to_owned()
    } else {
        lower
    };
    REASONING_LEVELS
        .iter()
        .copied()
        .find(|level| *level == lower)
}

fn is_word_token(token: &str) -> bool {
    let mut chars = token.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn with_level(base: &str, token: &str) -> Option<String> {
    let base = base.trim_end();
    if base.is_empty() {
        return None;
    }
    let level = known_level(token)?;
    if level == "max" && !base.contains('/') {
        return None;
    }
    Some(format!("{base}:{level}"))
}

/// Canonicalises a model string: `name (high)` / `name high` / `name:HIGH` become `name:high`
/// when the token is a known level (`none` reads as `off`). `:max` is kept only on
/// provider-qualified names. Anything else is returned unchanged.
pub fn canonical_model_string(model: &str) -> String {
    let text = model;
    if let Some(stripped) = text.trim_end().strip_suffix(')')
        && let Some(open) = stripped.rfind('(')
    {
        let base = &stripped[..open];
        if base.ends_with(char::is_whitespace)
            && let Some(out) = with_level(base, &stripped[open + 1..])
        {
            return out;
        }
        return model.to_owned();
    }
    if let Some(space) = text.trim_end().rfind(char::is_whitespace) {
        let trimmed = text.trim_end();
        let token = &trimmed[space + 1..];
        if is_word_token(token)
            && let Some(out) = with_level(&trimmed[..space], token)
        {
            return out;
        }
        return model.to_owned();
    }
    if let Some(colon) = text.rfind(':') {
        let (base, token) = (&text[..colon], &text[colon + 1..]);
        if let Some(out) = with_level(base, token) {
            return out;
        }
    }
    model.to_owned()
}

/// Folds the legacy fields of one model-bearing object into the canonical ones and removes them.
pub fn normalize_legacy_model_fields(object: &Map<String, Value>) -> Map<String, Value> {
    let mut out = object.clone();
    if let Some(Value::String(model)) = out.get("model") {
        let canonical = canonical_model_string(model);
        out.insert("model".into(), Value::String(canonical));
    }
    let thinking = object.get("thinking").and_then(Value::as_object);
    let thinking_type = thinking.and_then(|t| t.get("type")).and_then(Value::as_str);
    if !out.contains_key("reasoning") {
        let folded = object
            .get("reasoningEffort")
            .or_else(|| object.get("variant"))
            .cloned()
            .or_else(|| (thinking_type == Some("disabled")).then(|| Value::String("off".into())));
        if let Some(value) = folded {
            out.insert("reasoning".into(), value);
        }
    }
    let mut options = object
        .get("provider_options")
        .or_else(|| object.get("providerOptions"))
        .and_then(Value::as_object)
        .cloned();
    if let Some(thinking) = thinking
        && thinking_type == Some("enabled")
    {
        options
            .get_or_insert_with(Map::new)
            .insert("thinking".into(), Value::Object(thinking.clone()));
    }
    if let Some(verbosity) = object.get("textVerbosity") {
        options
            .get_or_insert_with(Map::new)
            .insert("textVerbosity".into(), verbosity.clone());
    }
    if let Some(options) = options {
        out.insert("provider_options".into(), Value::Object(options));
    }
    if let Some(max) = object.get("maxTokens").filter(|v| v.is_number()) {
        if !out.contains_key("max_tokens") {
            out.insert("max_tokens".into(), max.clone());
        }
        out.remove("maxTokens");
    }
    for field in LEGACY_MODEL_FIELDS {
        out.remove(field);
    }
    out
}

fn normalize_model_entry(entry: &mut Value) {
    match entry {
        Value::String(model) => *model = canonical_model_string(model),
        Value::Object(object) => *object = normalize_legacy_model_fields(object),
        _ => {}
    }
}

fn normalize_definition(definition: &mut Value) {
    let Value::Object(object) = definition else {
        return;
    };
    *object = normalize_legacy_model_fields(object);
    for key in ["models", "fallback_models"] {
        match object.get_mut(key) {
            Some(Value::Array(entries)) => entries.iter_mut().for_each(normalize_model_entry),
            Some(entry @ Value::String(_)) => normalize_model_entry(entry),
            _ => {}
        }
    }
}

/// Applies §4.5 to every model-bearing object of one section container (the root, a harness
/// block or a profile body).
pub fn normalize_section_container(container: &mut Map<String, Value>) {
    for section in ["agents", "categories", "model_profiles"] {
        if let Some(Value::Object(map)) = container.get_mut(section) {
            map.values_mut().for_each(normalize_definition);
        }
    }
    if let Some(Value::Object(catalog)) = container.get_mut("models") {
        for entry in catalog.values_mut() {
            if let Value::Object(object) = entry {
                *object = normalize_legacy_model_fields(object);
            }
        }
    }
}

/// Typed harness blocks (the `[opencode]` block is never interpreted).
pub const TYPED_HARNESS_BLOCKS: [&str; 3] = ["[native]", "[senpi]", "[codex]"];
/// Every harness block key.
pub const HARNESS_BLOCKS: [&str; 4] = ["[native]", "[senpi]", "[codex]", "[opencode]"];

/// Applies §4.5 across a whole document: top level, typed harness blocks, profiles and the
/// profiles' typed harness blocks.
pub fn normalize_document(root: &mut Value) {
    let Value::Object(root) = root else { return };
    normalize_with_blocks(root);
    if let Some(Value::Object(profiles)) = root.get_mut("profiles") {
        for profile in profiles.values_mut() {
            if let Value::Object(body) = profile {
                normalize_with_blocks(body);
            }
        }
    }
}

fn normalize_with_blocks(container: &mut Map<String, Value>) {
    normalize_section_container(container);
    for block in TYPED_HARNESS_BLOCKS {
        if let Some(Value::Object(inner)) = container.get_mut(block) {
            normalize_section_container(inner);
        }
    }
}
