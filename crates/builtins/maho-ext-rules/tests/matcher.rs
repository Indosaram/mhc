use maho_ext_rules::rules::{matcher::{MatcherCache, MatcherInput, normalize_globs, hash_content}, types::{RuleFrontmatter, MatchReason}};

#[test]
fn native_patterns_match_pinned_picomatch_generated_matrix() {
    let cases: serde_json::Value = serde_json::from_str(include_str!("matcher-source.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let pattern = case["pattern"].as_str().unwrap();
        let path = case["path"].as_str().unwrap();
        let frontmatter = RuleFrontmatter { globs: if pattern.starts_with('!') { vec!["**".into(), pattern.into()] } else { vec![pattern.into()] }, ..Default::default() };
        let result = MatcherCache::default().match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: false, project_relative: path, scope_relative: None, basename: path }).unwrap();
        let expected = case["matched"].as_bool().unwrap();
        assert_eq!(result.matched, expected, "{pattern}: {path}");
    }
}

#[test]
fn single_files_precede_always_apply_and_globs() {
    let frontmatter = RuleFrontmatter { always_apply: Some(true), ..Default::default() };
    let result = MatcherCache::default().match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: true, project_relative: "a", scope_relative: None, basename: "a" }).unwrap();
    assert_eq!(result.reason, MatchReason::SingleFile);
}
#[test]
fn glob_exclusion_prevents_basename_fallback() {
    let frontmatter = RuleFrontmatter { globs: vec!["**/*.rs".into(), "!src/excluded.rs".into()], ..Default::default() };
    let result = MatcherCache::default().match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: false, project_relative: "src/excluded.rs", scope_relative: None, basename: "excluded.rs" }).unwrap();
    assert!(!result.matched);
}
#[test]
fn matching_patterns_are_cached_and_resettable() {
    let frontmatter = RuleFrontmatter { globs: vec!["src/**/*.rs".into()], ..Default::default() };
    let mut cache = MatcherCache::default();
    for _ in 0..2 { assert!(cache.match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: false, project_relative: "src/a.rs", scope_relative: None, basename: "a.rs" }).unwrap().matched); }
    assert_eq!(cache.stats().entries, 1);
    assert_eq!(cache.stats().compiled_patterns, 1);
    cache.reset();
    assert_eq!(cache.stats().entries, 0);
}
#[test]
fn normalized_patterns_deduplicate_in_source_order() {
    let frontmatter = RuleFrontmatter { globs: vec!["src\\*.rs".into()], paths: vec!["src/*.rs".into()], apply_to: vec!["*.md".into()], ..Default::default() };
    assert_eq!(normalize_globs(&frontmatter), ["src/*.rs", "*.md"]);
    assert_eq!(hash_content(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
}
