use crate::{
    external_pattern::{ExternalPathMatcher, create_external_path_matcher},
    types::{Action, Rule},
    wildcard,
};

/// Last match wins over every ruleset; `external_directory` rules also match by filesystem
/// identity (senpi#4351) through [`create_external_path_matcher`].
pub fn evaluate(permission: &str, pattern: &str, rulesets: &[&[Rule]]) -> Rule {
    evaluate_patterns(permission, &[pattern], rulesets)
}

/// [`evaluate`] for a target with several spellings: a rule matching any of them matches.
pub fn evaluate_patterns(permission: &str, patterns: &[&str], rulesets: &[&[Rule]]) -> Rule {
    let matcher =
        (permission == "external_directory").then(|| create_external_path_matcher(false, None));
    evaluate_with(permission, patterns, rulesets, matcher.as_ref())
}

/// Evaluate a lexical spelling without letting a resolved rule erase its ancestry.
pub fn evaluate_external_spelling(
    patterns: &[&str],
    ruleset: &[Rule],
    canonical_case: Option<&dyn Fn(&str) -> String>,
) -> Rule {
    let matcher = create_external_path_matcher(true, canonical_case);
    evaluate_with("external_directory", patterns, &[ruleset], Some(&matcher))
}

fn evaluate_with(
    permission: &str,
    patterns: &[&str],
    rulesets: &[&[Rule]],
    external: Option<&ExternalPathMatcher<'_>>,
) -> Rule {
    rulesets
        .iter()
        .rev()
        .flat_map(|rules| rules.iter().rev())
        .find(|rule| {
            wildcard::matches(permission, &rule.permission)
                && patterns.iter().any(|pattern| {
                    matches_pattern(pattern, &rule.pattern)
                        || external.is_some_and(|external| external.matches(pattern, &rule.pattern))
                })
        })
        .cloned()
        .unwrap_or_else(|| Rule {
            action: Action::Ask,
            permission: permission.into(),
            pattern: "*".into(),
        })
}

fn matches_pattern(value: &str, pattern: &str) -> bool {
    wildcard::matches(value, pattern)
        || pattern
            .strip_suffix(" *")
            .is_some_and(|prefix| wildcard::matches(value, prefix))
}
