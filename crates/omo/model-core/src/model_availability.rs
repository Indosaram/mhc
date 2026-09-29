use std::sync::LazyLock;

use indexmap::IndexSet;
use regex::Regex;

#[expect(clippy::expect_used, reason = "static regex literals are valid")]
fn normalize_model_name(name: &str) -> String {
    static CLAUDE_VERSION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"claude-(opus|sonnet|haiku)-([0-9]+)[.-]([0-9]+)").expect("valid regex")
    });
    static KIMI_VERSION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"kimi-k2[.-]([0-9]+)").expect("valid regex"));
    static GLM_GPT_VERSION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?-u:\b)(glm|gpt)-([0-9]+)[.-]([0-9]+)").expect("valid regex")
    });

    let lowered = name.to_lowercase();
    let claude = CLAUDE_VERSION.replace_all(&lowered, "claude-$1-$2.$3");
    let kimi = KIMI_VERSION.replace_all(&claude, "kimi-k2.$1");
    GLM_GPT_VERSION.replace_all(&kimi, "$1-$2.$3").into_owned()
}

/// First shortest candidate, matching `reduce((a, b) => b.length < a.length ? b : a)`.
fn shortest<'a>(candidates: impl Iterator<Item = &'a String>) -> Option<&'a String> {
    candidates.reduce(|shortest, current| {
        if current.len() < shortest.len() {
            current
        } else {
            shortest
        }
    })
}

/// Exact match, then exact model-id match, then the shortest substring match.
#[must_use]
pub fn fuzzy_match_model(
    target: &str,
    available: &IndexSet<String>,
    providers: Option<&[String]>,
) -> Option<String> {
    let target_normalized = normalize_model_name(target);

    let candidates: Vec<&String> = match providers {
        Some(providers) if !providers.is_empty() => available
            .iter()
            .filter(|model| {
                let provider = model.split('/').next().unwrap_or_default();
                providers.iter().any(|entry| entry == provider)
            })
            .collect(),
        _ => available.iter().collect(),
    };

    let matches: Vec<&String> = candidates
        .into_iter()
        .filter(|model| normalize_model_name(model).contains(&target_normalized))
        .collect();

    if let Some(exact) = matches
        .iter()
        .find(|model| normalize_model_name(model) == target_normalized)
    {
        return Some((*exact).clone());
    }

    let exact_model_id_matches = matches.iter().copied().filter(|model| {
        let model_id = model.split_once('/').map_or("", |(_, model_id)| model_id);
        normalize_model_name(model_id) == target_normalized
    });
    if let Some(found) = shortest(exact_model_id_matches) {
        return Some(found.clone());
    }

    shortest(matches.into_iter()).cloned()
}

#[must_use]
pub fn is_model_available(target_model: &str, available_models: &IndexSet<String>) -> bool {
    fuzzy_match_model(target_model, available_models, None).is_some()
}
