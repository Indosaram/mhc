use std::collections::VecDeque;
use globset::{GlobBuilder, GlobMatcher};
use sha2::{Digest, Sha256};
use super::types::{MatchReason, RuleFrontmatter};

pub struct MatcherInput<'a> { pub frontmatter: &'a RuleFrontmatter, pub is_single_file: bool, pub project_relative: &'a str, pub scope_relative: Option<&'a str>, pub basename: &'a str }
pub struct MatchResult { pub matched: bool, pub reason: MatchReason }
struct PatternSet { key: String, positive: Vec<(String, GlobMatcher)>, negative: Vec<GlobMatcher> }
#[derive(Default)]
pub struct MatcherCache { sets: VecDeque<PatternSet> }
pub struct MatcherCacheStats { pub entries: usize, pub compiled_patterns: usize }
impl MatcherCache {
    pub fn reset(&mut self) { self.sets.clear(); }
    pub fn stats(&self) -> MatcherCacheStats { MatcherCacheStats { entries: self.sets.len(), compiled_patterns: self.sets.iter().map(|set| set.positive.len().saturating_add(set.negative.len())).sum() } }
    pub fn match_rule(&mut self, input: MatcherInput<'_>) -> Result<MatchResult, globset::Error> {
        if input.is_single_file { return Ok(MatchResult { matched: true, reason: MatchReason::SingleFile }); }
        if input.frontmatter.always_apply == Some(true) { return Ok(MatchResult { matched: true, reason: MatchReason::AlwaysApply }); }
        let patterns = normalize_globs(input.frontmatter);
        if patterns.is_empty() { return Ok(no_match()); }
        let key = patterns.join("\0");
        let cached = self.sets.iter().position(|set| set.key == key).and_then(|index| self.sets.remove(index));
        let set = match cached {
            Some(set) => set,
            None => {
                let mut set = PatternSet { key, positive: Vec::new(), negative: Vec::new() };
                for pattern in patterns {
                    let (negative, source) = pattern.strip_prefix('!').map_or((false, pattern.as_str()), |source| (true, source));
                    let matcher = GlobBuilder::new(source).literal_separator(false).backslash_escape(false).build()?.compile_matcher();
                    if negative { set.negative.push(matcher); } else { set.positive.push((pattern, matcher)); }
                }
                set
            }
        };
        let paths = [Some(input.project_relative), input.scope_relative.filter(|path| !path.is_empty()), Some(input.basename)].into_iter().flatten().map(|path| path.replace('\\', "/")).collect::<Vec<_>>();
        let mut result = no_match();
        'patterns: for (pattern, matcher) in &set.positive {
            for path in &paths {
                if matcher.is_match(path) {
                    if !set.negative.iter().any(|negative| negative.is_match(path)) { result = MatchResult { matched: true, reason: MatchReason::Glob { pattern: pattern.clone() } }; }
                    break 'patterns;
                }
            }
        }
        if self.sets.len() >= 256 { self.sets.pop_front(); }
        self.sets.push_back(set);
        Ok(result)
    }
}
pub fn normalize_globs(frontmatter: &RuleFrontmatter) -> Vec<String> {
    let mut patterns = Vec::new();
    for pattern in frontmatter.globs.iter().chain(&frontmatter.paths).chain(&frontmatter.apply_to) {
        let pattern = pattern.replace('\\', "/");
        if !patterns.contains(&pattern) { patterns.push(pattern); }
    }
    patterns
}
pub fn hash_content(body: &str) -> String { format!("{:x}", Sha256::digest(body.as_bytes())) }
fn no_match() -> MatchResult { MatchResult { matched: false, reason: MatchReason::NoMatch } }
