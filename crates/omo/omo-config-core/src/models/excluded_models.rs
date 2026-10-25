//! `exclude_models` matching. An entry with a `/` is matched against the whole `provider/model`; one
//! without is matched against the model id served by any provider. `*` matches any run of characters
//! (including none, and including `/`), and matching ignores case, so `*` keeps every Fable build out
//! whichever provider serves it. Blank entries match nothing.
//!
//! Every other character is literal: the TypeScript source escapes the regex metacharacters before
//! joining the `*`-split parts with `.*`, so `gpt-6.1-sol` matches only itself and never a
//! `.`-wildcard variant. The matcher here compares characters directly for exactly that reason - a
//! regex engine would have to re-escape the pattern on every call.

/// Upstream `patternExpression` as a matcher: anchored, case-insensitive, `*` matches any run.
fn pattern_matches(pattern: &str, subject: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let subject: Vec<char> = subject.to_lowercase().chars().collect();
    let (mut pattern_index, mut subject_index) = (0usize, 0usize);
    // The most recent `*`, and the subject position it was last expanded to.
    let mut star: Option<usize> = None;
    let mut star_subject = 0usize;
    while subject_index < subject.len() {
        match pattern.get(pattern_index) {
            Some('*') => {
                star = Some(pattern_index);
                star_subject = subject_index;
                pattern_index += 1;
            }
            Some(character) if *character == subject[subject_index] => {
                pattern_index += 1;
                subject_index += 1;
            }
            _ => {
                let Some(star_index) = star else {
                    return false;
                };
                pattern_index = star_index + 1;
                star_subject += 1;
                subject_index = star_subject;
            }
        }
    }
    pattern[pattern_index..].iter().all(|character| *character == '*')
}

/// Upstream `selectorTails`: every tail that starts after a `/`, or the selector itself when it has
/// none. A gateway's model id can itself be `vendor/model` (`openrouter/anthropic/`), so every tail
/// is a model id some provider serves.
fn selector_tails(model: &str) -> Vec<&str> {
    let tails: Vec<&str> = model
        .match_indices('/')
        .map(|(index, _)| &model[index + 1..])
        .collect();
    if tails.is_empty() { vec![model] } else { tails }
}

/// Upstream `excludedModelPattern`: the first pattern that excludes `model`, as the user wrote it.
pub fn excluded_model_pattern(model: &str, patterns: Option<&[String]>) -> Option<String> {
    let patterns = patterns?;
    if patterns.is_empty() {
        return None;
    }
    let tails = selector_tails(model);
    for raw in patterns {
        let pattern = raw.trim();
        if pattern.is_empty() {
            continue;
        }
        let matched = if pattern.contains('/') {
            pattern_matches(pattern, model)
                || tails.iter().any(|tail| pattern_matches(pattern, tail))
        } else {
            tails.iter().any(|tail| pattern_matches(pattern, tail))
        };
        if matched {
            return Some(raw.clone());
        }
    }
    None
}

/// Upstream `isExcludedModel`.
pub fn is_excluded_model(model: &str, patterns: Option<&[String]>) -> bool {
    excluded_model_pattern(model, patterns).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    /// `<family>-*`, built from parts so the wildcard is a runtime value.
    fn family_wildcard(family: &str) -> String {
        format!("{family}-{}", '*')
    }

    #[test]
    fn a_bare_model_id_keeps_that_model_out_on_every_provider() {
        let list = patterns(&["fable-5-1"]);
        assert_eq!(
            excluded_model_pattern("anthropic/fable-5-1", Some(&list)),
            Some("fable-5-1".to_owned())
        );
        assert_eq!(
            excluded_model_pattern("github-copilot/fable-5-1", Some(&list)),
            Some("fable-5-1".to_owned())
        );
        assert!(!is_excluded_model("anthropic/opus-5-5", Some(&list)));
    }

    #[test]
    fn a_provider_scoped_entry_keeps_out_only_that_providers_copy() {
        let list = patterns(&["opencode/fable-5-1"]);
        assert!(is_excluded_model("opencode/fable-5-1", Some(&list)));
        assert!(!is_excluded_model("anthropic/fable-5-1", Some(&list)));
    }

    #[test]
    fn a_wildcard_entry_matches_every_build_of_the_family_ignoring_case() {
        let list = patterns(&[family_wildcard("FABLE-5").as_str()]);
        assert!(is_excluded_model("anthropic/fable-5-1", Some(&list)));
        assert!(is_excluded_model("anthropic-subscription/FABLE-5-1", Some(&list)));
        assert!(!is_excluded_model("anthropic/opus-5-5", Some(&list)));
    }

    #[test]
    fn a_gateway_id_that_nests_a_provider_is_matched_on_its_tail() {
        let nested = "openrouter/anthropic/fable-5-1";
        assert!(is_excluded_model(nested, Some(&patterns(&[family_wildcard("fable-5").as_str()]))));
        assert!(is_excluded_model(nested, Some(&patterns(&["anthropic/fable-5-1"]))));
        assert!(is_excluded_model(nested, Some(&patterns(&["openrouter/anthropic/fable-5-1"]))));
        assert!(!is_excluded_model(nested, Some(&patterns(&["openrouter/"]))));
        assert!(!is_excluded_model(nested, Some(&patterns(&["anthropic"]))));
    }

    #[test]
    fn regex_metacharacters_and_blank_entries_are_literal_and_inert() {
        let list = patterns(&["  ", "gpt-6.1-sol"]);
        assert!(is_excluded_model("openai/gpt-6.1-sol", Some(&list)));
        assert!(!is_excluded_model("openai/gpt-6x1-sol", Some(&list)));
        assert!(!is_excluded_model("openai/gpt-6.1-sol", None));
        assert_eq!(
            excluded_model_pattern("openai/gpt-6.1-sol", Some(&patterns(&["gpt-6*", "gpt-6.1-sol"]))),
            Some("gpt-6*".to_owned())
        );
    }
}
