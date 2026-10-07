use maho_ext_rules::rules::{matcher::{MatcherCache, MatcherInput, normalize_globs, hash_content}, picomatch::{self, Options}, types::{RuleFrontmatter, MatchReason}};

fn match_patterns(patterns: &[&str], path: &str) -> bool {
    let frontmatter = RuleFrontmatter { globs: patterns.iter().map(|pattern| (*pattern).to_string()).collect(), ..Default::default() };
    MatcherCache::default().match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: false, project_relative: path, scope_relative: None, basename: path }).unwrap().matched
}

fn match_one(pattern: &str, path: &str) -> bool {
    match_patterns(&[pattern], path)
}

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
fn native_matcher_matches_source_generated_corpus() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/latest-omo-rules-matcher.json");
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("run `bun tools/golden/latest-omo-rules-matcher.mjs` to generate {path}: {error}"));
    let corpus: serde_json::Value = serde_json::from_str(&text).unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    assert!(!cases.is_empty(), "source-generated corpus must not be empty");
    for case in cases {
        let pattern = case["pattern"].as_str().unwrap();
        let path = case["path"].as_str().unwrap();
        let expected = case["matched"].as_bool().unwrap();
        let exclusion = case["exclusion"].as_bool().unwrap_or(false);
        let globs = if exclusion { vec!["**".to_string(), pattern.to_string()] } else { vec![pattern.to_string()] };
        let frontmatter = RuleFrontmatter { globs, ..Default::default() };
        let result = MatcherCache::default().match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: false, project_relative: path, scope_relative: None, basename: path }).unwrap();
        assert_eq!(result.matched, expected, "{pattern}: {path}");
    }
}

#[test]
fn pinned_extglob_quantifiers_match_source_verdicts() {
    assert!(match_one("@(foo|bar).ts", "foo.ts"));
    assert!(match_one("@(foo|bar).ts", "bar.ts"));
    assert!(!match_one("@(foo|bar).ts", "baz.ts"));
    assert!(match_one("+(foo|bar).ts", "foobar.ts"));
    assert!(!match_one("+(foo|bar).ts", ".ts"));
    assert!(match_one("?(foo|bar).ts", ".ts"));
    assert!(match_one("*(foo|bar).ts", "foofoo.ts"));
    assert!(match_one("src/@(foo|bar).ts", "src/bar.ts"));
}

#[test]
fn pinned_brace_expansion_and_ranges_match_source() {
    assert!(match_one("{a,b}", "a"));
    assert!(match_one("{a,b}", "b"));
    assert!(!match_one("{a,b}", "c"));
    assert!(match_one("{1..3}", "2"));
    assert!(match_one("{a..c}", "b"));
    assert!(match_one("a{b,c}", "ab"));
    assert!(match_one("{a,{b,c}}", "c"));
    assert!(match_one("{a}", "{a}"));
    assert!(!match_one("{a}", "a"));
    assert!(match_one("a{b,c", "a{b,c"));
}

#[test]
fn pinned_quoting_literalizes_magic() {
    assert!(match_one("\"*\"", "*"));
    assert!(!match_one("\"*\"", "abc"));
    assert!(match_one("\"a*b\"", "a*b"));
    assert!(!match_one("\"a*b\"", "axb"));
}

#[test]
fn pinned_posix_classes_match_source() {
    assert!(match_one("[[:alpha:]]", "a"));
    assert!(!match_one("[[:alpha:]]", "1"));
    assert!(match_one("[[:digit:]]", "7"));
    assert!(!match_one("[[:digit:]]", "a"));
    assert!(match_one("[[:space:]]", " "));
    assert!(match_one("@(a|b)[[:digit:]]", "a3"));
}

#[test]
fn pinned_globstar_crosses_directories() {
    assert!(match_one("a/**/b", "a/x/b"));
    assert!(match_one("a/**/b", "a/b"));
    assert!(!match_one("a/**/b", "a/x/c"));
    assert!(match_one("**", "a/b/c"));
    assert!(match_one("a/**", "a/b"));
    assert!(match_one("src/**/*.ts", "src/deep/nested/file.ts"));
}

#[test]
fn pinned_negative_globs_exclude_positives() {
    assert!(!match_patterns(&["**", "!foo"], "foo"));
    assert!(match_patterns(&["**", "!foo"], "bar"));
    assert!(!match_patterns(&["**/*.ts", "!**/foo.ts"], "src/foo.ts"));
    assert!(match_patterns(&["**/*.ts", "!**/foo.ts"], "src/bar.ts"));
    assert!(!match_patterns(&["**", "!(foo).ts"], "foo.ts"));
    assert!(match_patterns(&["**", "!(foo).ts"], "bar.ts"));
}

#[test]
fn pinned_dot_true_matches_dotfiles() {
    assert!(match_one("*", ".hidden"));
    assert!(match_one(".*", ".hidden"));
    assert!(match_one("**", ".a/b"));
}

#[test]
fn pinned_js_utf16_unit_semantics() {
    assert!(match_one("\u{1F600}", "\u{1F600}"));
    assert!(match_one("??", "\u{1F600}"));
    assert!(!match_one("?", "\u{1F600}"));
    assert!(match_one("?", "a"));
    assert!(match_one("@(a|b)?\u{1F600}", "b\u{1F600}"));
}

#[test]
fn pinned_extglob_astral_bodies_keep_utf16_unit_lengths() {
    // `analyzeRepeatedExtglob` measures each branch with `String#length`, i.e. UTF-16
    // code units, so an astral branch is never a single-character branch: it never
    // feeds the flat `[...]*` safe output and never counts as a repeated
    // single-character prefix. `+(*(a)|\u{1F600})` must therefore keep the pinned
    // escaped-literal fallback rather than collapsing to `[a\u{1F600}]*`.
    assert!(!match_one("+(*(a)|\u{1F600})", "a\u{1F600}"));
    assert!(!match_one("+(*(a)|\u{1F600})", "\u{1F600}"));
    assert!(!match_one("+(*(a)|\u{1F600})", "a"));
    assert!(!match_one("+(*(a)|\u{1F600})", "aa"));
    // `getStarExtglobSequenceChars` also rejects the branch (`length !== 1`), so the
    // nested star sequence is not flattened either.
    assert!(!match_one("+(*(\u{1F600}))", "\u{1F600}"));
    assert!(!match_one("+(*(\u{1F600}))", "\u{1F600}\u{1F600}"));
    // `hasRepeatedCharPrefixOverlap` compares against `char.repeat(a.length)` in
    // units, so repeated astral branches are not "the same character repeated"; the
    // extglob stays a live quantifier.
    assert!(match_one("+(\u{1F600}|\u{1F600})", "\u{1F600}"));
    assert!(match_one("+(\u{1F600}|\u{1F600})", "\u{1F600}\u{1F600}"));
    assert!(!match_one("+(\u{1F600}|\u{1F600})", "a"));
    // The all-single-unit safe output is unchanged for ASCII branches.
    assert!(match_one("+(*(a)|b)", "ab"));
    assert!(match_one("+(*(a)|b)", "ba"));
    assert!(!match_one("+(*(a)|b)", "c"));
    // Astral literals in the surrounding pattern keep matching unit by unit.
    assert!(match_one("@(a|\u{1F600})", "\u{1F600}"));
    assert!(match_one("+(a|\u{1F600})", "a\u{1F600}"));
    assert!(match_one("@(a|b)\u{1F600}", "b\u{1F600}"));
    assert!(!match_one("@(a|b)\u{1F600}", "b"));
    assert!(match_one("a\u{1F600}?", "a\u{1F600}x"));
}

/// Drive the ported parser directly: the pinned `normalizeGlobs` rewrites every `\` to
/// `/` before a glob reaches picomatch, so backslash-escape behavior is only observable
/// through the grammar module itself.
fn grammar_match(pattern: &str, path: &str) -> bool {
    picomatch::is_match(pattern, path, &Options::bash_dot())
}

#[test]
fn pinned_identity_and_octal_escapes_match_the_js_unit() {
    // JS (Annex B, no `u` flag) reads `\a`, `\z`, `\A`, `\e`, `\8`, `\<`, `\>` as the
    // literal character, while `regex-syntax` gives them a different meaning (`\a` is the
    // bell, `\z`/`\A` are text anchors, `\<`/`\>` are word-boundary assertions) or rejects
    // them; `\3` and `\12` are the JS legacy octal escapes and `\cA`/`\x41` the control
    // and hex escapes.
    assert!(grammar_match("\\a", "a"));
    assert!(!grammar_match("\\a", "\u{7}"));
    assert!(grammar_match("\\z", "z"));
    assert!(!grammar_match("\\z", "y"));
    assert!(grammar_match("\\A", "A"));
    assert!(grammar_match("\\e", "e"));
    assert!(grammar_match("\\8", "8"));
    assert!(grammar_match("\\<", "<"));
    assert!(grammar_match("\\>", ">"));
    assert!(grammar_match("\\3", "\u{3}"));
    assert!(!grammar_match("\\3", "3"));
    assert!(grammar_match("\\12", "\n"));
    assert!(grammar_match("\\cA", "\u{1}"));
    assert!(grammar_match("\\x41", "A"));
    assert!(grammar_match("a\\z\\3b", "az\u{3}b"));
}

#[test]
fn native_grammar_matches_source_generated_escape_corpus() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/latest-omo-rules-matcher.json");
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("run `bun tools/golden/latest-omo-rules-matcher.mjs` to generate {path}: {error}"));
    let corpus: serde_json::Value = serde_json::from_str(&text).unwrap();
    let cases = corpus["grammarCases"].as_array().unwrap_or_else(|| panic!("regenerate: `bun tools/golden/latest-omo-rules-matcher.mjs` writes grammarCases"));
    assert!(!cases.is_empty(), "source-generated grammar corpus must not be empty");
    for case in cases {
        let pattern = case["pattern"].as_str().unwrap();
        let path = case["path"].as_str().unwrap();
        let expected = case["matched"].as_bool().unwrap();
        assert_eq!(grammar_match(pattern, path), expected, "{pattern}: {path}");
    }
}

#[test]
fn pinned_expand_range_sorts_by_utf16_code_units() {
    // `Array#sort` orders the endpoints by UTF-16 code unit and the class is emitted in
    // the port's mapped unit domain, so `{😀..\u{E000}}` is `[D83D DE00-E000]`: it matches
    // U+E000 but not the two-unit string "😀".
    assert!(match_one("{\u{1F600}..\u{E000}}", "\u{E000}"));
    assert!(match_one("{\u{E000}..\u{1F600}}", "\u{E000}"));
    assert!(!match_one("{\u{1F600}..\u{E000}}", "\u{1F600}"));
    assert!(!match_one("{\u{1F600}..\u{E000}}", "a"));
    // The sorted order puts `a` first, so the class is `[a-😀]` = 0x61..0xD83D, which
    // covers ordinary BMP units and excludes U+E000.
    assert!(match_one("{\u{1F600}..a}", "z"));
    assert!(match_one("{a..\u{1F600}}", "z"));
    assert!(!match_one("{\u{1F600}..a}", "\u{E000}"));
}

#[test]
fn pinned_fastpath_patterns_match_source() {
    assert!(match_one("foo.ts", "foo.ts"));
    assert!(match_one("a?b", "axb"));
    assert!(!match_one("a?b", "ab"));
    assert!(match_one("a.b", "a.b"));
    assert!(match_one("a*b", "axb"));
    assert!(match_one("a*b", "a/b"));
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
fn cache_stays_bounded_across_unique_pattern_sets() {
    let mut cache = MatcherCache::default();
    for index in 0..300 {
        let pattern = format!("src/file-{index}.ts");
        let frontmatter = RuleFrontmatter { globs: vec![pattern.clone()], ..Default::default() };
        cache.match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: false, project_relative: &pattern, scope_relative: None, basename: &pattern }).unwrap();
    }
    assert!(cache.stats().entries <= 256);
}
#[test]
fn normalized_patterns_deduplicate_in_source_order() {
    let frontmatter = RuleFrontmatter { globs: vec!["src\\*.rs".into()], paths: vec!["src/*.rs".into()], apply_to: vec!["*.md".into()], ..Default::default() };
    assert_eq!(normalize_globs(&frontmatter), ["src/*.rs", "*.md"]);
    assert_eq!(hash_content(""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
}
#[test]
fn oversized_and_empty_patterns_keep_source_errors() {
    let long = "a".repeat(65537);
    let frontmatter = RuleFrontmatter { globs: vec![long], ..Default::default() };
    let error = MatcherCache::default().match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: false, project_relative: "a", scope_relative: None, basename: "a" }).unwrap_err();
    assert_eq!(error.to_string(), "Input length: 65537, exceeds maximum allowed length: 65536");
    let frontmatter = RuleFrontmatter { globs: vec![String::new()], ..Default::default() };
    let error = MatcherCache::default().match_rule(MatcherInput { frontmatter: &frontmatter, is_single_file: false, project_relative: "a", scope_relative: None, basename: "a" }).unwrap_err();
    assert_eq!(error.to_string(), "Expected pattern to be a non-empty string");
}
#[test]
fn malformed_patterns_stay_literal_like_source() {
    assert!(match_one("[abc", "[abc"));
    assert!(match_one("foo{,.txt}", "foo"));
    assert!(match_one("a{b", "a{b"));
}
