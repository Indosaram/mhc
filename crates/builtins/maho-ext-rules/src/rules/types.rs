#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleFrontmatter { pub description: Option<String>, pub globs: Vec<String>, pub paths: Vec<String>, pub apply_to: Vec<String>, pub always_apply: Option<bool> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedRule { pub frontmatter: RuleFrontmatter, pub body: String, pub diagnostic: Option<String> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchReason { AlwaysApply, SingleFile, Glob { pattern: String }, NoMatch }
#[derive(Clone, Debug)]
pub struct RuleCandidate { pub path: String, pub real_path: String, pub source: String, pub distance: usize, pub is_global: bool, pub is_single_file: bool, pub relative_path: String }
#[derive(Clone, Debug)]
pub struct LoadedRule { pub candidate: RuleCandidate, pub frontmatter: RuleFrontmatter, pub body: String, pub content_hash: String, pub match_reason: MatchReason }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RulesMode { Static, Dynamic, Both, Off }
#[derive(Clone, Debug)]
pub enum EnabledSources { Auto, Explicit(Vec<String>) }
#[derive(Clone, Debug)]
pub struct PiRulesConfig { pub disabled: bool, pub mode: RulesMode, pub max_rule_chars: usize, pub max_result_chars: usize, pub enabled_sources: EnabledSources }
#[derive(Clone, Debug)]
pub struct RuleDiagnostic { pub severity: String, pub source: String, pub message: String }
#[derive(Default)]
pub struct SessionState { pub cwd: Option<String>, pub static_dedup: std::collections::BTreeSet<String>, pub dynamic_dedup: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>, pub dynamic_target_fingerprints: std::collections::BTreeMap<String, String>, pub loaded_rules: Vec<LoadedRule>, pub diagnostics: Vec<RuleDiagnostic> }
