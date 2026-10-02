use crate::{types::{Action, Rule}, wildcard};

pub fn evaluate(permission: &str, pattern: &str, rulesets: &[&[Rule]]) -> Rule {
    rulesets.iter().rev().flat_map(|rules| rules.iter().rev()).find(|rule| {
        wildcard::matches(permission, &rule.permission)
            && (wildcard::matches(pattern, &rule.pattern) || rule.pattern.strip_suffix(" *").is_some_and(|prefix| wildcard::matches(pattern, prefix)))
    }).cloned().unwrap_or_else(|| Rule { action: Action::Ask, permission: permission.into(), pattern: "*".into() })
}
