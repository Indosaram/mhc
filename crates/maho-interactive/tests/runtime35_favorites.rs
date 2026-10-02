use maho_ai::types::ModelThinkingLevel;
use maho_core::model_resolver::PatternResolution;
use maho_interactive::components::model_favorites::*;

fn resolution(pattern: &str, ids: &[&str]) -> PatternResolution {
    PatternResolution { pattern: pattern.into(), owned_ids: ids.iter().map(|id| (*id).into()).collect(), thinking_level: Some(ModelThinkingLevel::High), service_tier: Some("priority".into()), unresolved: false, is_glob: pattern.contains('*') }
}

fn merge(patterns: &[&str], resolutions: &[PatternResolution], selected: Option<&[&str]>, candidates: &[&str]) -> Option<Vec<String>> {
    let patterns: Vec<_> = patterns.iter().map(|id| (*id).into()).collect();
    let selected = selected.map(|ids| ids.iter().map(|id| (*id).into()).collect());
    let candidates: Vec<_> = candidates.iter().map(|id| (*id).into()).collect();
    merge_favorite_patterns_for_persist(FavoritePatternsForPersist { stored_patterns: &patterns, pattern_resolutions: resolutions, selected_ids: &selected, candidate_ids: &candidates })
}

#[test]
fn unchanged_decorated_pattern_remains_verbatim_with_unrelated_selection() {
    let result = merge(&["p/*:priority:high"], &[resolution("p/*:priority:high", &["p/a", "p/b"])], Some(&["p/a", "p/b", "q/c"]), &["p/a", "p/b", "q/c"]);
    assert_eq!(result, Some(vec!["p/*:priority:high".into(), "q/c".into()]));
}

#[test]
fn partially_deselected_glob_retains_decorators_and_invisible_owned_ids() {
    let result = merge(&["p/*"], &[resolution("p/*", &["p/a", "p/b", "p/c"])], Some(&["p/b"]), &["p/a", "p/b"]);
    assert_eq!(result, Some(vec!["p/b:priority:high".into(), "p/c:priority:high".into()]));
}

#[test]
fn exact_pattern_can_be_removed_and_refavorited_without_losing_decoration() {
    let resolutions = [resolution("p/a:high", &["p/a"])];
    assert_eq!(merge(&["p/a:high"], &resolutions, Some(&[]), &["p/a"]), None);
    assert_eq!(merge(&["p/a:high"], &resolutions, Some(&["p/a"]), &["p/a"]), Some(vec!["p/a:high".into()]));
}

#[test]
fn unresolved_patterns_survive_empty_selection() {
    let mut missing = resolution("missing:*", &[]);
    missing.unresolved = true;
    assert_eq!(merge(&["missing:*"], &[missing], Some(&[]), &["p/a"]), Some(vec!["missing:*".into()]));
}

#[test]
fn overlapping_pattern_with_no_ownership_cannot_resurrect_deselected_id() {
    assert_eq!(merge(&["p/a:high", "a"], &[resolution("p/a:high", &["p/a"]), resolution("a", &[])], Some(&[]), &["p/a"]), None);
}

#[test]
fn disappearing_snapshot_ids_preserve_their_pattern() {
    assert_eq!(merge(&["p/a:high"], &[resolution("p/a:high", &["p/a"])], Some(&["q/new"]), &["q/new"]), Some(vec!["p/a:high".into(), "q/new".into()]));
}

#[test]
fn inserted_stored_pattern_does_not_shift_resolution_identity() {
    assert_eq!(merge(&["new", "p/a:high"], &[resolution("p/a:high", &["p/a"])], Some(&["p/a"]), &["p/a"]), Some(vec!["p/a:high".into()]));
}

#[test]
fn removed_stored_pattern_does_not_shift_resolution_identity() {
    assert_eq!(merge(&["p/a:high"], &[resolution("removed", &["q/b"]), resolution("p/a:high", &["p/a"])], Some(&["p/a", "q/b"]), &["p/a", "q/b"]), Some(vec!["p/a:high".into(), "q/b".into()]));
}

#[test]
fn reordered_stored_patterns_retain_their_own_decorators() {
    assert_eq!(merge(&["q/b:high", "p/a:high"], &[resolution("p/a:high", &["p/a"]), resolution("q/b:high", &["q/b"])], Some(&["p/a", "q/b"]), &["p/a", "q/b"]), Some(vec!["q/b:high".into(), "p/a:high".into()]));
}

#[test]
fn empty_resolution_snapshot_appends_selected_ids_bare() {
    assert_eq!(merge(&["p/a:high"], &[], Some(&["p/a"]), &["p/a"]), Some(vec!["p/a".into()]));
}

#[test]
fn duplicate_stored_patterns_share_first_resolution_and_are_deduplicated() {
    assert_eq!(merge(&["p/a:high", "p/a:high"], &[resolution("p/a:high", &["p/a"]), resolution("p/a:high", &[])], Some(&["p/a"]), &["p/a"]), Some(vec!["p/a:high".into()]));
}
