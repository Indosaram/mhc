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

fn model_id(model: &str) -> &str {
    model.split_once('/').map_or("", |(_, model_id)| model_id)
}

fn provider_rank(model: &str, providers: Option<&[String]>) -> usize {
    providers
        .filter(|providers| !providers.is_empty())
        .map_or(0, |providers| {
            providers
                .iter()
                .position(|provider| model.split('/').next() == Some(provider.as_str()))
                .unwrap_or(usize::MAX)
        })
}

fn closest<'a>(
    candidates: impl Iterator<Item = &'a String>,
    providers: Option<&[String]>,
) -> Option<&'a String> {
    candidates.reduce(|best, current| {
        let best_rank = provider_rank(best, providers);
        let current_rank = provider_rank(current, providers);
        if current_rank < best_rank
            || (current_rank == best_rank && model_id(current).len() < model_id(best).len())
        {
            current
        } else {
            best
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
    if let Some(found) = closest(exact_model_id_matches, providers) {
        return Some(found.clone());
    }

    closest(matches.into_iter(), providers).cloned()
}

#[must_use]
pub fn is_model_available(target_model: &str, available_models: &IndexSet<String>) -> bool {
    fuzzy_match_model(target_model, available_models, None).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unqualified_candidates_have_empty_model_id_for_ties() {
        for candidates in [
            ["provider/target-short", "target-with-a-long-unqualified-suffix"],
            ["target-with-a-long-unqualified-suffix", "provider/target-short"],
        ] {
            let available = candidates.into_iter().map(str::to_string).collect();
            assert_eq!(fuzzy_match_model("target", &available, None).as_deref(), Some("target-with-a-long-unqualified-suffix"));
        }
    }

    #[test]
    fn upstream_normalization_and_provider_preference_cases() {
        for (target, candidate, provider) in [
            ("kimi-k2.6", "moonshot/kimi-k2-6", "moonshot"),
            ("glm-5-1", "zai/glm-5.1", "zai"),
            ("gpt-5.4", "openai/gpt-5-4", "openai"),
        ] {
            let available = IndexSet::from([candidate.to_string()]);
            assert_eq!(fuzzy_match_model(target, &available, Some(&[provider.to_string()])).as_deref(), Some(candidate));
        }
        let available = IndexSet::from([
            "anthropic-subscription/claude-opus-5".to_string(),
            "opencode/claude-opus-5".to_string(),
        ]);
        assert_eq!(fuzzy_match_model("claude-opus-5", &available, None).as_deref(), Some("anthropic-subscription/claude-opus-5"));
        for providers in [["anthropic-subscription", "opencode"], ["opencode", "anthropic-subscription"]] {
            let expected = format!("{}/claude-opus-5", providers[0]);
            let providers = providers.map(str::to_string);
            assert_eq!(fuzzy_match_model("claude-opus-5", &available, Some(&providers)).as_deref(), Some(expected.as_str()));
        }
        let available = IndexSet::from([
            "opencode/claude-opus-5-2025".to_string(),
            "anthropic-subscription/claude-opus-5".to_string(),
        ]);
        assert_eq!(fuzzy_match_model("claude-opus", &available, None).as_deref(), Some("anthropic-subscription/claude-opus-5"));
    }
}
