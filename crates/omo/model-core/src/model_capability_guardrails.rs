use std::collections::BTreeSet;

use indexmap::IndexSet;

use crate::model_capabilities::ModelCapabilitiesSnapshot;
use crate::model_capability_aliases::AliasSource;
use crate::model_capability_aliases::get_exact_model_id_alias_rules;
use crate::model_capability_aliases::get_pattern_model_id_alias_rules;
use crate::model_capability_aliases::resolve_model_id_alias;
use crate::model_requirements::AGENT_MODEL_REQUIREMENTS;
use crate::model_requirements::CATEGORY_MODEL_REQUIREMENTS;

/// A drift between alias rules, built-in requirements, and the capability snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelCapabilityGuardrailIssue {
    AliasTargetMissingFromSnapshot {
        rule_id: String,
        alias_model_id: String,
        canonical_model_id: String,
        message: String,
    },
    ExactAliasCollidesWithSnapshot {
        rule_id: String,
        alias_model_id: String,
        canonical_model_id: String,
        message: String,
    },
    PatternAliasCollidesWithSnapshot {
        rule_id: String,
        model_id: String,
        canonical_model_id: String,
        message: String,
    },
    BuiltInModelReliesOnAlias {
        model_id: String,
        canonical_model_id: String,
        rule_id: String,
        message: String,
    },
    BuiltInModelMissingFromSnapshot {
        model_id: String,
        canonical_model_id: String,
        message: String,
    },
}

impl ModelCapabilityGuardrailIssue {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AliasTargetMissingFromSnapshot { .. } => "alias-target-missing-from-snapshot",
            Self::ExactAliasCollidesWithSnapshot { .. } => "exact-alias-collides-with-snapshot",
            Self::PatternAliasCollidesWithSnapshot { .. } => "pattern-alias-collides-with-snapshot",
            Self::BuiltInModelReliesOnAlias { .. } => "built-in-model-relies-on-alias",
            Self::BuiltInModelMissingFromSnapshot { .. } => "built-in-model-missing-from-snapshot",
        }
    }
}

#[derive(Default)]
pub struct CollectModelCapabilityGuardrailIssuesInput<'a> {
    pub snapshot: Option<&'a ModelCapabilitiesSnapshot>,
    pub load_bundled_snapshot: Option<&'a dyn Fn() -> ModelCapabilitiesSnapshot>,
    pub requirement_model_ids: Option<Vec<String>>,
}

/// Every model id referenced by a built-in agent or category chain, sorted and unique.
#[must_use]
pub fn get_built_in_requirement_model_ids() -> Vec<String> {
    AGENT_MODEL_REQUIREMENTS
        .values()
        .chain(CATEGORY_MODEL_REQUIREMENTS.values())
        .flat_map(|requirement| {
            requirement
                .fallback_chain
                .iter()
                .map(|entry| entry.model.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[must_use]
pub fn collect_model_capability_guardrail_issues(
    input: CollectModelCapabilityGuardrailIssuesInput<'_>,
) -> Vec<ModelCapabilityGuardrailIssue> {
    let loaded;
    let snapshot = match (input.snapshot, input.load_bundled_snapshot) {
        (Some(snapshot), _) => snapshot,
        (None, Some(load)) => {
            loaded = load();
            &loaded
        }
        (None, None) => return Vec::new(),
    };
    let snapshot_model_ids: IndexSet<String> = snapshot
        .models
        .keys()
        .map(|model_id| model_id.trim().to_lowercase())
        .collect();
    let requirement_model_ids = input
        .requirement_model_ids
        .unwrap_or_else(get_built_in_requirement_model_ids);
    let mut issues = Vec::new();

    for rule in get_exact_model_id_alias_rules() {
        if !snapshot_model_ids.contains(rule.canonical_model_id) {
            issues.push(
                ModelCapabilityGuardrailIssue::AliasTargetMissingFromSnapshot {
                    rule_id: rule.rule_id.to_string(),
                    alias_model_id: rule.alias_model_id.to_string(),
                    canonical_model_id: rule.canonical_model_id.to_string(),
                    message: format!(
                        "Alias {} points to missing snapshot model {}.",
                        rule.alias_model_id, rule.canonical_model_id
                    ),
                },
            );
        }
        if snapshot_model_ids.contains(rule.alias_model_id) {
            issues.push(ModelCapabilityGuardrailIssue::ExactAliasCollidesWithSnapshot {
                rule_id: rule.rule_id.to_string(),
                alias_model_id: rule.alias_model_id.to_string(),
                canonical_model_id: rule.canonical_model_id.to_string(),
                message: format!(
                    "Alias {} now exists in models.dev and should be reviewed instead of force-mapping to {}.",
                    rule.alias_model_id, rule.canonical_model_id
                ),
            });
        }
    }

    for rule in get_pattern_model_id_alias_rules() {
        for model_id in &snapshot_model_ids {
            if !(rule.matches)(model_id) {
                continue;
            }
            let canonical_model_id = (rule.canonicalize)(model_id);
            if canonical_model_id == *model_id {
                continue;
            }
            issues.push(ModelCapabilityGuardrailIssue::PatternAliasCollidesWithSnapshot {
                rule_id: rule.rule_id.to_string(),
                model_id: model_id.clone(),
                message: format!(
                    "Pattern alias {} would rewrite canonical snapshot model {model_id} to {canonical_model_id}.",
                    rule.rule_id
                ),
                canonical_model_id,
            });
        }
    }

    for model_id in &requirement_model_ids {
        let alias_resolution = resolve_model_id_alias(model_id, None);
        if alias_resolution.source != AliasSource::Canonical {
            let rule_id = alias_resolution
                .rule_id
                .clone()
                .unwrap_or_else(|| "unknown-alias-rule".to_string());
            issues.push(ModelCapabilityGuardrailIssue::BuiltInModelReliesOnAlias {
                model_id: alias_resolution.requested_model_id.clone(),
                canonical_model_id: alias_resolution.canonical_model_id.clone(),
                message: format!(
                    "Built-in requirement model {} should be canonical and not rely on alias rule {}.",
                    alias_resolution.requested_model_id,
                    alias_resolution.rule_id.as_deref().unwrap_or("undefined")
                ),
                rule_id,
            });
        }
        if !snapshot_model_ids.contains(&alias_resolution.canonical_model_id) {
            issues.push(ModelCapabilityGuardrailIssue::BuiltInModelMissingFromSnapshot {
                message: format!(
                    "Built-in requirement model {} resolves to {}, which is missing from the bundled snapshot.",
                    alias_resolution.requested_model_id, alias_resolution.canonical_model_id
                ),
                model_id: alias_resolution.requested_model_id,
                canonical_model_id: alias_resolution.canonical_model_id,
            });
        }
    }

    issues
}
