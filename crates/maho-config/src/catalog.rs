//! Model catalog alias resolution inside a view (spec §8).

use crate::diagnostics::{Diagnostic, DiagnosticKind, DiagnosticTarget};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashSet};

/// Built-in model profile ids (users' files name them).
pub const BUILTIN_MODEL_PROFILE_IDS: [&str; 4] =
    ["daily-normal", "daily-heavy", "geeky-normal", "geeky-heavy"];

struct Alias {
    model: String,
    reasoning: Option<Value>,
}

struct Catalog {
    aliases: Map<String, Value>,
    cyclic: HashSet<String>,
}

impl Catalog {
    fn lookup(&self, name: &str) -> Option<Alias> {
        if self.cyclic.contains(name) {
            return None;
        }
        let entry = self.aliases.get(name)?.as_object()?;
        Some(Alias {
            model: entry.get("model")?.as_str()?.to_owned(),
            reasoning: entry.get("reasoning").filter(|r| !r.is_null()).cloned(),
        })
    }

    fn resolve_list_entry(&self, entry: &mut Value) {
        match entry {
            Value::String(name) => {
                if let Some(alias) = self.lookup(name) {
                    *entry = match alias.reasoning {
                        Some(reasoning) => json!({"model": alias.model, "reasoning": reasoning}),
                        None => Value::String(alias.model),
                    };
                }
            }
            Value::Object(object) => self.resolve_definition(object),
            _ => {}
        }
    }

    fn resolve_definition(&self, object: &mut Map<String, Value>) {
        let Some(alias) = object
            .get("model")
            .and_then(Value::as_str)
            .and_then(|m| self.lookup(m))
        else {
            return;
        };
        object.insert("model".into(), Value::String(alias.model));
        if !object.contains_key("reasoning")
            && let Some(reasoning) = alias.reasoning
        {
            object.insert("reasoning".into(), reasoning);
        }
    }

    fn resolve_fallbacks(&self, value: &mut Value) {
        match value {
            Value::String(name) => {
                if let Some(alias) = self.lookup(name) {
                    *value = match alias.reasoning {
                        Some(reasoning) => json!([{"model": alias.model, "reasoning": reasoning}]),
                        None => Value::String(alias.model),
                    };
                }
            }
            Value::Array(entries) => entries
                .iter_mut()
                .for_each(|entry| self.resolve_list_entry(entry)),
            _ => {}
        }
    }
}

fn find_cycles(aliases: &Map<String, Value>) -> Vec<(String, bool)> {
    let next = |name: &str| -> Option<String> {
        let target = aliases.get(name)?.get("model")?.as_str()?;
        aliases.contains_key(target).then(|| target.to_owned())
    };
    let mut out = Vec::new();
    let names: BTreeSet<&String> = aliases.keys().collect();
    for name in names {
        let mut seen = HashSet::new();
        let mut current = next(name);
        let mut steps = 0;
        while let Some(step) = current {
            steps += 1;
            if step == *name {
                out.push((name.clone(), steps == 1));
                break;
            }
            if !seen.insert(step.clone()) {
                break;
            }
            current = next(&step);
        }
    }
    out
}

/// Resolves catalog references in `view`; returns the new view and the catalog diagnostics.
pub fn resolve_catalog_references(view: &Value) -> (Value, Vec<Diagnostic>) {
    let mut out = view.clone();
    let mut diagnostics = Vec::new();
    let aliases = view
        .get("models")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let cycles = find_cycles(&aliases);
    for (name, self_reference) in &cycles {
        let path = format!("models.{name}.model");
        let message = if *self_reference {
            format!("{path}: the alias \"{name}\" refers to itself and is not resolved")
        } else {
            format!("{path}: the alias \"{name}\" is part of a reference cycle and is not resolved")
        };
        diagnostics.push(Diagnostic::new(
            DiagnosticKind::CatalogCycle,
            DiagnosticTarget::Config(path.clone()),
            message,
            vec![path],
        ));
    }
    let catalog = Catalog {
        cyclic: cycles.into_iter().map(|(name, _)| name).collect(),
        aliases,
    };

    let mut profile_ids: Vec<String> = BUILTIN_MODEL_PROFILE_IDS
        .iter()
        .map(|id| (*id).to_owned())
        .collect();
    if let Some(profiles) = view.get("model_profiles").and_then(Value::as_object) {
        for id in profiles.keys() {
            if catalog.aliases.contains_key(id) {
                let path = format!("model_profiles.{id}");
                diagnostics.push(Diagnostic::new(
                    DiagnosticKind::Validation,
                    DiagnosticTarget::Config(path.clone()),
                    format!("{path}: the model profile id \"{id}\" is also a model alias; the alias wins in model references"),
                    vec![path],
                ));
            }
            if !profile_ids.contains(id) {
                profile_ids.push(id.clone());
            }
        }
    }
    for section in ["categories", "agents"] {
        let Some(definitions) = view.get(section).and_then(Value::as_object) else {
            continue;
        };
        for (name, definition) in definitions {
            let Some(models) = definition.get("models").and_then(Value::as_array) else {
                continue;
            };
            for (index, entry) in models.iter().enumerate() {
                if let Some(text) = entry.as_str()
                    && profile_ids.iter().any(|id| id == text)
                    && !catalog.aliases.contains_key(text)
                {
                    let path = format!("{section}.{name}.models.{index}");
                    diagnostics.push(Diagnostic::new(
                        DiagnosticKind::Validation,
                        DiagnosticTarget::Config(path.clone()),
                        format!("{path}: \"{text}\" is a model profile; model profiles cannot be used inside a model chain"),
                        vec![path],
                    ));
                }
            }
        }
    }

    for section in ["agents", "categories"] {
        if let Some(Value::Object(definitions)) = out.get_mut(section) {
            for definition in definitions.values_mut() {
                let Value::Object(object) = definition else {
                    continue;
                };
                catalog.resolve_definition(object);
                if let Some(Value::Array(entries)) = object.get_mut("models") {
                    entries
                        .iter_mut()
                        .for_each(|entry| catalog.resolve_list_entry(entry));
                }
                if section == "categories"
                    && let Some(fallbacks) = object.get_mut("fallback_models")
                {
                    catalog.resolve_fallbacks(fallbacks);
                }
            }
        }
    }
    if let Some(Value::Object(profiles)) = out.get_mut("model_profiles") {
        for profile in profiles.values_mut() {
            if let Some(Value::Array(entries)) = profile.get_mut("models") {
                entries
                    .iter_mut()
                    .for_each(|entry| catalog.resolve_list_entry(entry));
            }
        }
    }
    (out, diagnostics)
}
