use std::collections::VecDeque;
use sha2::{Digest, Sha256};
use super::picomatch::{self, Options};
use super::types::{MatchReason, RuleFrontmatter};

pub struct MatcherInput<'a> { pub frontmatter: &'a RuleFrontmatter, pub is_single_file: bool, pub project_relative: &'a str, pub scope_relative: Option<&'a str>, pub basename: &'a str }

#[derive(Debug, PartialEq, Eq)]
pub struct MatchResult { pub matched: bool, pub reason: MatchReason }

#[derive(Debug, PartialEq, Eq)]
pub struct MatcherCacheStats { pub entries: usize, pub compiled_patterns: usize }

struct PatternSet { key: String, positive: Vec<(String, picomatch::Compiled)>, negative: Vec<picomatch::Compiled> }

#[derive(Debug)]
pub enum MatcherError { EmptyPattern, Syntax(String) }

impl std::fmt::Display for MatcherError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPattern => formatter.write_str("Expected pattern to be a non-empty string"),
            Self::Syntax(message) => formatter.write_str(message),
        }
    }
}
impl std::error::Error for MatcherError {}

impl From<picomatch::PicomatchError> for MatcherError {
    fn from(error: picomatch::PicomatchError) -> Self {
        match error {
            picomatch::PicomatchError::Type(_) => Self::EmptyPattern,
            picomatch::PicomatchError::Syntax(message) => Self::Syntax(message),
        }
    }
}

#[derive(Default)]
pub struct MatcherCache { sets: VecDeque<PatternSet> }

pub fn normalize_globs(frontmatter: &RuleFrontmatter) -> Vec<String> {
    let mut patterns = Vec::new();
    for pattern in frontmatter.globs.iter().chain(&frontmatter.paths).chain(&frontmatter.apply_to) {
        let pattern = pattern.replace('\\', "/");
        if !patterns.contains(&pattern) { patterns.push(pattern); }
    }
    patterns
}
pub fn hash_content(body: &str) -> String { format!("{:x}", Sha256::digest(body.as_bytes())) }

fn create_glob_matcher(pattern: &str) -> Result<picomatch::Compiled, MatcherError> {
    picomatch::compile(pattern, &Options::bash_dot()).map_err(MatcherError::from)
}

impl MatcherCache {
    pub fn reset(&mut self) { self.sets.clear(); }
    pub fn stats(&self) -> MatcherCacheStats { MatcherCacheStats { entries: self.sets.len(), compiled_patterns: self.sets.iter().map(|set| set.positive.len() + set.negative.len()).sum() } }
    pub fn match_rule(&mut self, input: MatcherInput<'_>) -> Result<MatchResult, MatcherError> {
        if input.is_single_file { return Ok(MatchResult { matched: true, reason: MatchReason::SingleFile }); }
        if input.frontmatter.always_apply == Some(true) { return Ok(MatchResult { matched: true, reason: MatchReason::AlwaysApply }); }
        let patterns = normalize_globs(input.frontmatter);
        if patterns.is_empty() { return Ok(no_match()); }
        let key = patterns.join("\0");
        let set = if let Some(index) = self.sets.iter().position(|set| set.key == key) {
            self.sets.remove(index).unwrap_or_else(|| unreachable!("position belongs to this deque"))
        } else {
            let mut set = PatternSet { key, positive: Vec::new(), negative: Vec::new() };
            for pattern in patterns {
                let negated = pattern.starts_with('!');
                let value = pattern.strip_prefix('!').unwrap_or(&pattern);
                let compiled = create_glob_matcher(value)?;
                if negated { set.negative.push(compiled); } else { set.positive.push((pattern, compiled)); }
            }
            if self.sets.len() >= 256 { self.sets.pop_front(); }
            set
        };
        self.sets.push_back(set);
        let set = self.sets.back().unwrap_or_else(|| unreachable!("set was just inserted"));
        let bases = [Some(input.project_relative), input.scope_relative.filter(|value| !value.is_empty()), Some(input.basename)];
        for (pattern, matcher) in &set.positive {
            for base in bases.into_iter().flatten() {
                let base = base.replace('\\', "/");
                if !base.is_empty() && (base == *pattern || matcher.is_match(&base)) {
                    if set.negative.iter().any(|negative| negative.is_match(&base)) { return Ok(no_match()); }
                    return Ok(MatchResult { matched: true, reason: MatchReason::Glob { pattern: pattern.clone() } });
                }
            }
        }
        Ok(no_match())
    }
}

fn no_match() -> MatchResult { MatchResult { matched: false, reason: MatchReason::NoMatch } }
