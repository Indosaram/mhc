//! `exclude_models` registry view (`model-exclusion.ts`).
//!
//! Also the crate's home for the `excluded-models.ts` matcher: `withoutExcludedModels` is the only
//! thing that needs it, and both the category and agent resolvers walk registries through it.

use serde_json::Value;

use crate::host::{HostError, SenpiModelRegistry};

/// `provider/model-id` of one registry entry (`model-exclusion.ts` `modelKey`); `None` when the entry
/// is not an object carrying both string fields.
fn registry_model_key(model: &Value) -> Option<String> {
    let object = model.as_object()?;
    let provider = object.get("provider")?.as_str()?;
    let id = object.get("id")?.as_str()?;
    Some(format!("{provider}/{id}"))
}

/// `excluded-models.ts` `patternExpression`: `*` matches any run of characters (including `/` and
/// none at all) and every other character is literal; matching ignores case.
fn wildcard_pattern_matches(pattern: &str, subject: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let subject: Vec<char> = subject.to_lowercase().chars().collect();
    let (mut p, mut s) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while s < subject.len() {
        if p < pattern.len() && pattern[p] != '*' && pattern[p] == subject[s] {
            p += 1;
            s += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            mark = s;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            mark += 1;
            s = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// `excluded-models.ts` `selectorTails`: every tail starting after a `/` is a model id some provider
/// serves, so an entry is tested against each of them.
fn selector_tails(model: &str) -> Vec<&str> {
    let tails: Vec<&str> = model
        .match_indices('/')
        .map(|(index, _)| &model[index + 1..])
        .collect();
    if tails.is_empty() { vec![model] } else { tails }
}

/// The entry that keeps `model` out, or `None` (`excluded-models.ts` `excludedModelPattern`).
pub fn excluded_model_pattern<'a>(model: &str, patterns: &'a [String]) -> Option<&'a str> {
    let tails = selector_tails(model);
    for raw in patterns {
        let pattern = raw.trim();
        if pattern.is_empty() {
            continue;
        }
        let matched = if pattern.contains('/') {
            wildcard_pattern_matches(pattern, model)
                || tails
                    .iter()
                    .any(|tail| wildcard_pattern_matches(pattern, tail))
        } else {
            tails
                .iter()
                .any(|tail| wildcard_pattern_matches(pattern, tail))
        };
        if matched {
            return Some(raw.as_str());
        }
    }
    None
}

/// `excluded-models.ts` `isExcludedModel`: whether any pattern keeps `model` out of a registry view.
pub fn is_excluded_model(model: &str, patterns: &[String]) -> bool {
    excluded_model_pattern(model, patterns).is_some()
}

/// The registry as `exclude_models` leaves it (`model-exclusion.ts` `withoutExcludedModels`): an
/// excluded model is neither available nor found, so every chain walk skips it exactly as it skips a
/// model whose provider has no login. With no patterns every call delegates unchanged.
pub struct ExcludedModelsRegistry<'a> {
    inner: &'a dyn SenpiModelRegistry,
    patterns: Vec<String>,
}

impl SenpiModelRegistry for ExcludedModelsRegistry<'_> {
    fn get_available(&self) -> Result<Value, HostError> {
        let available = self.inner.get_available()?;
        let Value::Array(models) = available else {
            return Ok(available);
        };
        Ok(Value::Array(
            models
                .into_iter()
                .filter(|model| match registry_model_key(model) {
                    None => true,
                    Some(key) => !is_excluded_model(&key, &self.patterns),
                })
                .collect(),
        ))
    }

    fn find(&self, provider: &str, model_id: &str) -> Option<Value> {
        if is_excluded_model(&format!("{provider}/{model_id}"), &self.patterns) {
            return None;
        }
        self.inner.find(provider, model_id)
    }
}

/// `withoutExcludedModels`: the registry with `patterns` applied.
pub fn without_excluded_models<'a>(
    registry: &'a dyn SenpiModelRegistry,
    patterns: Option<&[String]>,
) -> ExcludedModelsRegistry<'a> {
    ExcludedModelsRegistry {
        inner: registry,
        patterns: patterns.map(<[String]>::to_vec).unwrap_or_default(),
    }
}
