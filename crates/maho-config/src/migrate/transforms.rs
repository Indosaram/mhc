//! The document transforms of the startup plans (spec §12.2).

use crate::legacy_names::{rename_legacy_categories, rename_legacy_harness};
use crate::model_string::{canonical_model_string, normalize_legacy_model_fields};
use serde_json::{Map, Value, json};

/// Legacy provider ids and their replacements (interface facts).
pub const PROVIDER_RENAMES: [(&str, &str); 2] = [
    ("claude-sdk-oauth", "anthropic-subscription"),
    ("openai-codex", "chatgpt-subscription"),
];

const REASONING_FIELDS: [&str; 3] = ["reasoning", "reasoningEffort", "variant"];
const CHAIN_DROPPED: [&str; 9] = [
    "model",
    "fallback_models",
    "reasoning",
    "variant",
    "reasoningEffort",
    "provider_options",
    "providerOptions",
    "thinking",
    "textVerbosity",
];

fn resolve_conflicts(object: &mut Map<String, Value>, path: &str, diagnostics: &mut Vec<String>) {
    let present: Vec<&str> = REASONING_FIELDS
        .iter()
        .copied()
        .filter(|f| object.contains_key(*f))
        .collect();
    if present.len() > 1 {
        let kept = present[0];
        for dropped in &present[1..] {
            diagnostics.push(format!(
                "conflict: {path} dropped {dropped}={} kept {kept}={}",
                object[*dropped], object[kept]
            ));
            object.shift_remove(*dropped);
        }
    }
}

fn normalize_entry(
    entry: &mut Value,
    path: &str,
    diagnostics: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    match entry {
        Value::String(model) => {
            if model.trim().is_empty() {
                errors.push(format!("{path}: empty model reference"));
            }
            *model = canonical_model_string(model);
        }
        Value::Object(object) => {
            resolve_conflicts(object, path, diagnostics);
            *object = normalize_legacy_model_fields(object);
            if object
                .get("model")
                .and_then(Value::as_str)
                .is_none_or(|m| m.trim().is_empty())
            {
                errors.push(format!(
                    "{path}: a model entry needs a non-empty \"model\" string"
                ));
            }
        }
        _ => errors.push(format!("{path}: not a model reference")),
    }
}

fn entries(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(text)) => vec![Value::String(text.clone())],
        _ => Vec::new(),
    }
}

fn primary_entry(definition: &Map<String, Value>) -> Option<Value> {
    let model = definition.get("model")?.as_str()?;
    let mut entry = Map::new();
    entry.insert("model".into(), Value::String(model.to_owned()));
    for key in ["reasoning", "provider_options"] {
        if let Some(value) = definition.get(key) {
            entry.insert(key.into(), value.clone());
        }
    }
    Some(if entry.len() == 1 {
        Value::String(model.to_owned())
    } else {
        Value::Object(entry)
    })
}

fn normalize_definition(
    definition: &mut Value,
    is_agent: bool,
    path: &str,
    diagnostics: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    let Value::Object(object) = definition else {
        return;
    };
    resolve_conflicts(object, path, diagnostics);
    *object = normalize_legacy_model_fields(object);
    for key in ["models", "fallback_models"] {
        match object.get_mut(key) {
            Some(Value::Array(items)) => {
                for (index, item) in items.iter_mut().enumerate() {
                    normalize_entry(item, &format!("{path}.{key}.{index}"), diagnostics, errors);
                }
            }
            Some(item @ Value::String(_)) => {
                normalize_entry(item, &format!("{path}.{key}"), diagnostics, errors)
            }
            _ => {}
        }
    }
    let fold = object.contains_key("fallback_models")
        || (is_agent && object.contains_key("model") && object.contains_key("models"));
    if fold {
        let mut chain = Vec::new();
        chain.extend(primary_entry(object));
        if is_agent {
            chain.extend(entries(object.get("models")));
        }
        chain.extend(entries(object.get("fallback_models")));
        for key in CHAIN_DROPPED {
            object.shift_remove(key);
        }
        object.insert("models".into(), Value::Array(chain));
    }
}

fn normalize_container(
    container: &mut Map<String, Value>,
    path: &str,
    diagnostics: &mut Vec<String>,
    errors: &mut Vec<String>,
) {
    let at = |section: &str, name: &str| {
        if path.is_empty() {
            format!("{section}.{name}")
        } else {
            format!("{path}.{section}.{name}")
        }
    };
    for (section, is_agent) in [("agents", true), ("categories", false)] {
        if let Some(Value::Object(definitions)) = container.get_mut(section) {
            for (name, definition) in definitions.iter_mut() {
                normalize_definition(
                    definition,
                    is_agent,
                    &at(section, name),
                    diagnostics,
                    errors,
                );
            }
        }
    }
    if let Some(Value::Object(profiles)) = container.get_mut("model_profiles") {
        for (name, profile) in profiles.iter_mut() {
            if let Some(Value::Array(items)) = profile.get_mut("models") {
                for (index, item) in items.iter_mut().enumerate() {
                    normalize_entry(
                        item,
                        &format!("{}.models.{index}", at("model_profiles", name)),
                        diagnostics,
                        errors,
                    );
                }
            }
        }
    }
    if let Some(Value::Object(catalog)) = container.get_mut("models") {
        for (name, entry) in catalog.iter_mut() {
            let entry_path = at("models", name);
            match entry {
                Value::Object(object) => {
                    resolve_conflicts(object, &entry_path, diagnostics);
                    *object = normalize_legacy_model_fields(object);
                }
                Value::String(model) => {
                    *entry = json!({"model": canonical_model_string(model)});
                }
                _ => {}
            }
        }
    }
}

/// Like the read-time folding, but `maxTokens`, `providerOptions` and `textVerbosity` stay as
/// written (the `[opencode]` block belongs to another product).
fn normalize_opencode(block: &mut Map<String, Value>) {
    fn keep(object: &mut Map<String, Value>) {
        let original = object.clone();
        let mut out = normalize_legacy_model_fields(&original);
        for key in ["maxTokens", "providerOptions", "textVerbosity"] {
            if let Some(value) = original.get(key) {
                out.insert(key.into(), value.clone());
            }
        }
        for key in ["provider_options", "max_tokens"] {
            if !original.contains_key(key) {
                out.shift_remove(key);
            }
        }
        *object = out;
    }
    for section in ["agents", "categories"] {
        if let Some(Value::Object(definitions)) = block.get_mut(section) {
            for definition in definitions.values_mut() {
                if let Value::Object(object) = definition {
                    keep(object);
                }
            }
        }
    }
}

/// Plan `2026-08-reasoning-unification`.
pub fn reasoning_unification(document: &Value) -> Result<(Value, Vec<String>), String> {
    let mut out = document.clone();
    let mut diagnostics = Vec::new();
    let mut errors = Vec::new();
    let Value::Object(root) = &mut out else {
        return Err("the document is not an object".into());
    };
    let blocks = ["[native]", "[senpi]", "[codex]"];
    normalize_container(root, "", &mut diagnostics, &mut errors);
    for block in blocks {
        if let Some(Value::Object(inner)) = root.get_mut(block) {
            normalize_container(inner, block, &mut diagnostics, &mut errors);
        }
    }
    if let Some(Value::Object(inner)) = root.get_mut("[opencode]") {
        normalize_opencode(inner);
    }
    if let Some(Value::Object(profiles)) = root.get_mut("profiles") {
        for (name, body) in profiles.iter_mut() {
            let Value::Object(body) = body else { continue };
            let base = format!("profiles.{name}");
            normalize_container(body, &base, &mut diagnostics, &mut errors);
            for block in blocks {
                if let Some(Value::Object(inner)) = body.get_mut(block) {
                    normalize_container(
                        inner,
                        &format!("{base}.{block}"),
                        &mut diagnostics,
                        &mut errors,
                    );
                }
            }
            if let Some(Value::Object(inner)) = body.get_mut("[opencode]") {
                normalize_opencode(inner);
            }
        }
    }
    if !errors.is_empty() {
        return Err(format!(
            "cannot normalise model references: {}",
            errors.join("; ")
        ));
    }
    Ok((out, diagnostics))
}

/// Plan `2026-09-category-deep-split`.
pub fn category_split(document: &Value) -> (Value, Vec<String>) {
    let mut out = document.clone();
    let diagnostics = rename_legacy_categories(&mut out)
        .into_iter()
        .map(|rename| {
            if rename.renamed {
                format!("{}: renamed deep to deep-low", rename.path)
            } else {
                format!("{}: dropped (deep-low already exists)", rename.path)
            }
        })
        .collect();
    (out, diagnostics)
}

/// Plan `2026-09-harness-native-rename`.
pub fn harness_rename(document: &Value) -> (Value, Vec<String>) {
    let mut out = document.clone();
    let diagnostics = rename_legacy_harness(&mut out)
        .into_iter()
        .map(|rename| {
            if rename.renamed {
                format!("{}: renamed to [native]", rename.path)
            } else {
                format!("{}: dropped ([native] already exists)", rename.path)
            }
        })
        .collect();
    (out, diagnostics)
}

/// The renamed form of a provider id or `provider/…` string, if it needs renaming.
pub fn renamed_provider(text: &str) -> Option<String> {
    PROVIDER_RENAMES.iter().find_map(|(from, to)| {
        if text == *from {
            Some((*to).to_owned())
        } else {
            text.strip_prefix(&format!("{from}/"))
                .map(|rest| format!("{to}/{rest}"))
        }
    })
}

/// Whether any string value or object key needs a provider rename.
pub fn needs_provider_rename(value: &Value) -> bool {
    match value {
        Value::String(text) => renamed_provider(text).is_some(),
        Value::Array(items) => items.iter().any(needs_provider_rename),
        Value::Object(map) => map
            .iter()
            .any(|(k, v)| renamed_provider(k).is_some() || needs_provider_rename(v)),
        _ => false,
    }
}

/// Plan `2026-09-subscription-provider-rename`.
pub fn provider_rename(document: &Value) -> (Value, Vec<String>) {
    fn walk(value: &Value, path: &str, diagnostics: &mut Vec<String>) -> Value {
        match value {
            Value::String(text) => match renamed_provider(text) {
                Some(to) => {
                    diagnostics.push(format!("{path}: {text} renamed to {to}"));
                    Value::String(to)
                }
                None => value.clone(),
            },
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| walk(item, &format!("{path}[{index}]"), diagnostics))
                    .collect(),
            ),
            Value::Object(map) => {
                let mut out = Map::new();
                for (key, child) in map {
                    let new_key = match renamed_provider(key) {
                        Some(to) if !map.contains_key(&to) => {
                            diagnostics.push(format!("{path}.{key}: {key} renamed to {to}"));
                            to
                        }
                        _ => key.clone(),
                    };
                    let child_path = format!("{path}.{new_key}");
                    out.insert(new_key, walk(child, &child_path, diagnostics));
                }
                Value::Object(out)
            }
            other => other.clone(),
        }
    }
    let mut diagnostics = Vec::new();
    let out = walk(document, "$", &mut diagnostics);
    (out, diagnostics)
}

/// Import transform of `2026-07-codex-config-jsonc`: copies `[opencode]` and `[codex]` as-is and
/// `[senpi]` (else `[omo]`) into `[native]`; the sidecar history goes to `legacy_migrations`.
pub fn codex_import(source: Option<&Value>, sidecar: Option<&Value>) -> (Value, Vec<String>) {
    let mut out = Map::new();
    let mut diagnostics = Vec::new();
    if let Some(source) = source.and_then(Value::as_object) {
        for block in ["[opencode]", "[codex]"] {
            if let Some(inner) = source.get(block) {
                out.insert(block.into(), inner.clone());
            }
        }
        match (source.get("[senpi]"), source.get("[omo]")) {
            (Some(senpi), Some(_)) => {
                diagnostics.push("conflict: both [senpi] and [omo] are present; [senpi] was imported into [native]".into());
                out.insert("[native]".into(), senpi.clone());
            }
            (Some(block), None) | (None, Some(block)) => {
                out.insert("[native]".into(), block.clone());
            }
            (None, None) => {}
        }
        if let Some(schema) = source.get("$schema") {
            out.insert("$schema".into(), schema.clone());
        }
    }
    if let Some(history) = sidecar {
        out.insert("legacy_migrations".into(), json!({"config.jsonc": history}));
    }
    (Value::Object(out), diagnostics)
}
