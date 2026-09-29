use model_core::CollectModelCapabilityGuardrailIssuesInput;
use model_core::ModelCapabilitiesSnapshotEntry;
use model_core::ModelCapabilityGuardrailIssue;
use model_core::collect_model_capability_guardrail_issues;
use model_core::get_built_in_requirement_model_ids;
use pretty_assertions::assert_eq;

use crate::support::bundled_snapshot;

#[test]
fn keeps_luna_fast_aligned_with_its_bundled_canonical_model() {
    let snapshot = bundled_snapshot();

    let issues =
        collect_model_capability_guardrail_issues(CollectModelCapabilityGuardrailIssuesInput {
            snapshot: Some(&snapshot),
            ..Default::default()
        });

    assert!(!issues.iter().any(|issue| matches!(
        issue,
        ModelCapabilityGuardrailIssue::BuiltInModelMissingFromSnapshot { model_id, .. } if model_id == "gpt-5.6-luna-fast"
    )));
}

#[test]
fn requires_built_in_requirement_models_to_stay_unique_and_sorted() {
    let model_ids = get_built_in_requirement_model_ids();

    let mut sorted = model_ids.clone();
    sorted.sort();
    assert_eq!(model_ids, sorted);
    let unique: std::collections::HashSet<_> = model_ids.iter().collect();
    assert_eq!(unique.len(), model_ids.len());
    assert!(model_ids.iter().any(|id| id == "claude-opus-5"));
    assert!(!model_ids.iter().any(|id| id == "gpt-5.5"));
    assert!(model_ids.iter().any(|id| id == "gpt-5.6-sol"));
    assert!(model_ids.iter().any(|id| id == "kimi-k3"));
}

#[test]
fn flags_exact_aliases_whose_canonical_target_disappears_from_the_snapshot() {
    let mut broken_snapshot = bundled_snapshot();
    broken_snapshot.models.shift_remove("gemini-3-pro-preview");

    let issues =
        collect_model_capability_guardrail_issues(CollectModelCapabilityGuardrailIssuesInput {
            snapshot: Some(&broken_snapshot),
            requirement_model_ids: Some(Vec::new()),
            ..Default::default()
        });

    assert!(issues.iter().any(|issue| matches!(
        issue,
        ModelCapabilityGuardrailIssue::AliasTargetMissingFromSnapshot { alias_model_id, canonical_model_id, .. }
            if alias_model_id == "gemini-3-pro-high" && canonical_model_id == "gemini-3-pro-preview"
    )));
}

fn with_gemini_alias_entry(model_id: &str) -> model_core::ModelCapabilitiesSnapshot {
    let mut snapshot = bundled_snapshot();
    snapshot.models.insert(
        model_id.to_string(),
        ModelCapabilitiesSnapshotEntry {
            id: model_id.to_string(),
            family: Some("gemini".to_string()),
            reasoning: Some(true),
            ..Default::default()
        },
    );
    snapshot
}

#[test]
fn flags_pattern_aliases_when_models_dev_gains_a_canonical_entry_for_the_alias_itself() {
    let snapshot = with_gemini_alias_entry("gemini-3.1-pro-high");

    let issues =
        collect_model_capability_guardrail_issues(CollectModelCapabilityGuardrailIssuesInput {
            snapshot: Some(&snapshot),
            requirement_model_ids: Some(Vec::new()),
            ..Default::default()
        });

    assert!(issues.iter().any(|issue| matches!(
        issue,
        ModelCapabilityGuardrailIssue::PatternAliasCollidesWithSnapshot { model_id, canonical_model_id, .. }
            if model_id == "gemini-3.1-pro-high" && canonical_model_id == "gemini-3.1-pro"
    )));
}

#[test]
fn flags_exact_aliases_when_models_dev_gains_a_canonical_entry_for_the_alias_itself() {
    let snapshot = with_gemini_alias_entry("gemini-3-pro-high");

    let issues =
        collect_model_capability_guardrail_issues(CollectModelCapabilityGuardrailIssuesInput {
            snapshot: Some(&snapshot),
            requirement_model_ids: Some(Vec::new()),
            ..Default::default()
        });

    assert!(issues.iter().any(|issue| matches!(
        issue,
        ModelCapabilityGuardrailIssue::ExactAliasCollidesWithSnapshot { alias_model_id, canonical_model_id, .. }
            if alias_model_id == "gemini-3-pro-high" && canonical_model_id == "gemini-3-pro-preview"
    )));
}

#[test]
fn flags_built_in_requirement_models_that_rely_on_aliases_instead_of_canonical_ids() {
    let snapshot = bundled_snapshot();

    let issues =
        collect_model_capability_guardrail_issues(CollectModelCapabilityGuardrailIssuesInput {
            snapshot: Some(&snapshot),
            requirement_model_ids: Some(vec!["gemini-3.1-pro-high".to_string()]),
            ..Default::default()
        });

    assert!(issues.iter().any(|issue| matches!(
        issue,
        ModelCapabilityGuardrailIssue::BuiltInModelReliesOnAlias { model_id, canonical_model_id, rule_id, .. }
            if model_id == "gemini-3.1-pro-high" && canonical_model_id == "gemini-3.1-pro" && rule_id == "gemini-3.1-pro-tier-alias"
    )));
}
