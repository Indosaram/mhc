#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleFrontmatter {
    pub description: Option<String>,
    pub globs: Option<PatternList>,
    pub paths: Option<PatternList>,
    pub apply_to: Option<PatternList>,
    pub always_apply: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternList {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedRule {
    pub frontmatter: RuleFrontmatter,
    pub body: String,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleCandidate {
    pub path: String,
    pub real_path: String,
    pub source: String,
    pub distance: usize,
    pub is_global: bool,
    pub is_single_file: bool,
    pub relative_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchReason {
    AlwaysApply,
    SingleFile,
    Glob { pattern: String },
    NoMatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedRule {
    pub candidate: RuleCandidate,
    pub frontmatter: RuleFrontmatter,
    pub body: String,
    pub content_hash: String,
    pub match_reason: MatchReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleDiagnostic {
    pub severity: Severity,
    pub source: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity { Warning, Error }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode { Static, Dynamic, Both, Off }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnabledSources { Auto, Sources(Vec<String>) }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiRulesConfig {
    pub disabled: bool,
    pub mode: Mode,
    pub max_rule_chars: usize,
    pub max_result_chars: usize,
    pub enabled_sources: EnabledSources,
}
