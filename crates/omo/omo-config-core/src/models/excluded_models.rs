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
