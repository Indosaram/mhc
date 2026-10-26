//! `exclude_models` matching.
//!
//! Port of `packages/omo-config-core/src/models/excluded-models.ts`. An entry with a `/` is
//! matched against the whole `provider/model`; one without is matched against the model id served
//! by any provider. `*` matches any run of characters (including none, and including `/`), and
//! matching ignores case, so `*claude*` keeps every Claude build out whichever provider serves it.
//! Blank entries match nothing.

use regex::{Regex, RegexBuilder};

/// Escapes the regex syntax the TS `patternExpression` escapes. `*` is deliberately left alone: it
/// is the wildcard the caller splits on before escaping each literal run.
fn escape_literal(part: &str) -> String {
    let mut escaped = String::with_capacity(part.len());
    for character in part.chars() {
        if matches!(
            character,
            '.' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn pattern_expression(pattern: &str) -> Option<Regex> {
    let escaped = pattern
        .split('*')
        .map(escape_literal)
        .collect::<Vec<_>>()
        .join(".*");
    RegexBuilder::new(&format!("^{escaped}$"))
        .case_insensitive(true)
        .build()
        .ok()
}

/// A selector is `provider/modelId`, and a gateway's model id can itself be `vendor/model`
/// (`openrouter/anthropic/`). Every tail that starts after a `/` is a model id some provider
/// serves, so an entry is tested against each of them; a `provider/model` entry may also match the
/// whole selector.
fn selector_tails(model: &str) -> Vec<&str> {
    let mut tails: Vec<&str> = Vec::new();
    for (index, character) in model.char_indices() {
        if character == '/' {
            tails.push(&model[index + 1..]);
        }
    }
    if tails.is_empty() {
        return vec![model];
    }
    tails
}

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
        let Some(expression) = pattern_expression(pattern) else {
            continue;
        };
        let matched = if pattern.contains('/') {
            std::iter::once(model)
                .chain(tails.iter().copied())
                .any(|subject| expression.is_match(subject))
        } else {
            tails.iter().copied().any(|subject| expression.is_match(subject))
        };
        if matched {
            return Some(raw.clone());
        }
    }
    None
}

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
