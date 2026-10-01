use std::{collections::{BTreeSet, VecDeque}, path::{Path, PathBuf}};
use super::{cache, constants::PROJECT_SINGLE_FILES, finder::{FinderOptions, RuleDiscoveryCache, find_rule_candidates}, formatter::{FormatOptions, format_static_block, format_dynamic_block}, matcher::{MatcherCache, MatcherInput, hash_content}, ordering::sort_candidates, parser::parse_rule, project_root::find_project_root, types::*};

pub struct LoadResult { pub rules: Vec<LoadedRule>, pub diagnostics: Vec<RuleDiagnostic> }
pub struct DynamicTargetFingerprint { pub target_path: PathBuf, pub cache_key: String, pub fingerprint: String }
pub struct Engine { pub state: SessionState, pub config: PiRulesConfig, home_dir: PathBuf, matcher: MatcherCache, dynamic_matches: VecDeque<(String, Option<MatchReason>)> }
impl Engine {
    pub fn new(config: PiRulesConfig, home_dir: PathBuf) -> Self { Self { state: SessionState::default(), config, home_dir, matcher: MatcherCache::default(), dynamic_matches: VecDeque::new() } }
    pub fn reset_session(&mut self, cwd: Option<&str>) {
        cache::clear_session(&mut self.state);
        self.dynamic_matches.clear();
        if let Some(cwd) = cwd { self.state.cwd = Some(cwd.into()); }
    }
    fn disabled_sources(&self) -> BTreeSet<String> {
        let EnabledSources::Explicit(enabled) = &self.config.enabled_sources else { return BTreeSet::new(); };
        super::constants::SOURCE_PRIORITY.iter().map(|(source, _)| *source).filter(|source| !enabled.iter().any(|entry| entry == source)).map(str::to_owned).collect()
    }
    fn store(&mut self, result: LoadResult) -> LoadResult {
        self.state.loaded_rules = result.rules.clone();
        self.state.diagnostics = result.diagnostics.clone();
        result
    }
    pub fn load_static_rules(&mut self, cwd: &Path) -> LoadResult {
        self.state.cwd = Some(cwd.to_string_lossy().into_owned());
        let mut result = LoadResult { rules: Vec::new(), diagnostics: Vec::new() };
        if self.config.disabled || matches!(self.config.mode, RulesMode::Off | RulesMode::Dynamic) { return self.store(result); }
        let root = find_project_root(cwd, None);
        let disabled = self.disabled_sources();
        let candidates = find_rule_candidates(FinderOptions { project_root: root.as_deref(), target_file: None, home_dir: &self.home_dir, disabled_sources: &disabled, skip_user_home: false }, &mut RuleDiscoveryCache::default());
        let mut selected_root_single = false;
        for candidate in sort_candidates(&candidates) {
            let root_single = candidate.distance == 0 && candidate.is_single_file && PROJECT_SINGLE_FILES.contains(&candidate.source.as_str()) && !candidate.source.contains('/');
            if root_single && selected_root_single { continue; }
            let Some(mut rule) = load_candidate(candidate, root.as_deref(), &mut result.diagnostics) else { continue; };
            rule.match_reason = if rule.frontmatter.always_apply == Some(true) { MatchReason::AlwaysApply } else if rule.candidate.is_single_file { MatchReason::SingleFile } else { continue; };
            if root_single { selected_root_single = true; }
            result.rules.push(rule);
        }
        self.store(result)
    }
    pub fn load_dynamic_rules(&mut self, cwd: &Path, targets: &[PathBuf]) -> Result<LoadResult, globset::Error> {
        self.state.cwd = Some(cwd.to_string_lossy().into_owned());
        let mut result = LoadResult { rules: Vec::new(), diagnostics: Vec::new() };
        if self.config.disabled || matches!(self.config.mode, RulesMode::Off | RulesMode::Static) { return Ok(self.store(result)); }
        let disabled = self.disabled_sources();
        let mut discovery = RuleDiscoveryCache::default();
        let mut seen_targets = BTreeSet::new();
        let mut seen_rules = BTreeSet::new();
        for target in targets {
            if !seen_targets.insert(target) { continue; }
            let root = find_project_root(target, None);
            let candidates = find_rule_candidates(FinderOptions { project_root: root.as_deref(), target_file: Some(target), home_dir: &self.home_dir, disabled_sources: &disabled, skip_user_home: false }, &mut discovery);
            for candidate in sort_candidates(&candidates) {
                let Some(mut rule) = load_candidate(candidate, root.as_deref(), &mut result.diagnostics) else { continue; };
                let basename = target.file_name().unwrap_or_default().to_string_lossy();
                let relative = root.as_deref().map(|root| relative_path(root, target)).unwrap_or_else(|| basename.to_string());
                let scope = if rule.candidate.is_global { None } else if rule.candidate.is_single_file { Path::new(&rule.candidate.path).parent().map(Path::to_path_buf) } else {
                    root.as_ref().map(|root| rule.candidate.relative_path.find(&rule.candidate.source).map_or_else(|| root.clone(), |index| root.join(rule.candidate.relative_path[..index].trim_end_matches('/'))))
                };
                let scope_relative = scope.as_deref().map(|scope| relative_path(scope, target));
                let key = [root.as_ref().map_or_else(String::new, |root| root.to_string_lossy().into_owned()), super::finder::absolute(target).to_string_lossy().replace('\\', "/"), rule.candidate.real_path.clone(), rule.candidate.relative_path.clone(), rule.candidate.source.clone(), if rule.candidate.is_global { "global" } else { "project" }.into(), if rule.candidate.is_single_file { "single" } else { "multi" }.into(), rule.candidate.distance.to_string(), rule.content_hash.clone()].join("\0");
                let reason = if let Some(index) = self.dynamic_matches.iter().position(|(cached, _)| cached == &key) {
                    let cached = self.dynamic_matches.remove(index).expect("existing cache index");
                    let reason = cached.1.clone();
                    self.dynamic_matches.push_back(cached);
                    reason
                } else {
                    let matched = self.matcher.match_rule(MatcherInput { frontmatter: &rule.frontmatter, is_single_file: rule.candidate.is_single_file, project_relative: &relative, scope_relative: scope_relative.as_deref(), basename: &basename })?;
                    let reason = matched.matched.then_some(matched.reason);
                    if self.dynamic_matches.len() >= 4096 { self.dynamic_matches.pop_front(); }
                    self.dynamic_matches.push_back((key, reason.clone()));
                    reason
                };
                let Some(reason) = reason else { continue; };
                if !seen_rules.insert(format!("{}::{}", rule.candidate.real_path, rule.content_hash)) { continue; }
                rule.match_reason = reason;
                result.rules.push(rule);
            }
        }
        result.rules.sort_by(|a, b| super::ordering::compare_candidates(&a.candidate, &b.candidate));
        Ok(self.store(result))
    }
    pub fn fingerprint_dynamic_targets(&mut self, cwd: &Path, targets: &[PathBuf]) -> Vec<DynamicTargetFingerprint> {
        use std::os::unix::fs::MetadataExt;
        self.state.cwd = Some(cwd.to_string_lossy().into_owned());
        if self.config.disabled || matches!(self.config.mode, RulesMode::Off | RulesMode::Static) { return Vec::new(); }
        let disabled = self.disabled_sources();
        let mut discovery = RuleDiscoveryCache::default();
        let cwd_root = find_project_root(cwd, None);
        let mut seen = BTreeSet::new();
        let mut result = Vec::new();
        for target in targets {
            if !seen.insert(target) { continue; }
            let root = if cwd_root.as_deref().is_some_and(|root| target.starts_with(root)) { cwd_root.clone() } else { find_project_root(target, None) };
            let candidates = find_rule_candidates(FinderOptions { project_root: root.as_deref(), target_file: Some(target), home_dir: &self.home_dir, disabled_sources: &disabled, skip_user_home: false }, &mut discovery);
            let candidate_fingerprints = sort_candidates(&candidates).into_iter().map(|candidate| {
                let stat = std::fs::metadata(&candidate.path).map_or_else(|_| "missing".into(), |stat| format!("{}:{}:{}", i128::from(stat.mtime()) * 1_000_000_000 + i128::from(stat.mtime_nsec()), i128::from(stat.ctime()) * 1_000_000_000 + i128::from(stat.ctime_nsec()), stat.len()));
                [candidate.real_path, candidate.relative_path, candidate.source, if candidate.is_global { "global" } else { "project" }.into(), if candidate.is_single_file { "single" } else { "multi" }.into(), candidate.distance.to_string(), stat].join("\0")
            }).collect::<Vec<_>>().join("\u{1}");
            let cache_key = super::finder::absolute(target).to_string_lossy().replace('\\', "/");
            let sources = match &self.config.enabled_sources { EnabledSources::Auto => "auto".into(), EnabledSources::Explicit(sources) => sources.join(",") };
            let fingerprint = hash_content(&["v1".into(), sources, root.map_or_else(String::new, |root| root.to_string_lossy().into_owned()), cache_key.clone(), candidate_fingerprints].join("\0"));
            result.push(DynamicTargetFingerprint { target_path: target.clone(), cache_key, fingerprint });
        }
        result
    }
    pub fn is_dynamic_target_fingerprint_current(&self, target: &DynamicTargetFingerprint) -> bool { self.state.dynamic_target_fingerprints.get(&target.cache_key) == Some(&target.fingerprint) }
    pub fn commit_dynamic_target_fingerprints(&mut self, targets: &[DynamicTargetFingerprint]) {
        for target in targets { self.state.dynamic_target_fingerprints.insert(target.cache_key.clone(), target.fingerprint.clone()); }
    }
    pub fn format_static(&self, rules: &[LoadedRule]) -> String { format_static_block(rules, &self.format_options()) }
    pub fn format_dynamic(&self, rules: &[LoadedRule], target: &str) -> String { format_dynamic_block(rules, target, &self.format_options()) }
    fn format_options(&self) -> FormatOptions { FormatOptions { max_rule_chars: self.config.max_rule_chars, max_result_chars: self.config.max_result_chars } }
}
pub fn relative_path(base: &Path, target: &Path) -> String {
    let base = super::finder::absolute(base);
    let target = super::finder::absolute(target);
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = target.components().collect();
    let shared = base.iter().zip(&target).take_while(|(base, target)| base == target).count();
    std::iter::repeat_n("..".to_owned(), base.len() - shared).chain(target[shared..].iter().map(|component| component.as_os_str().to_string_lossy().into_owned())).collect::<Vec<_>>().join("/")
}
fn load_candidate(candidate: RuleCandidate, root: Option<&Path>, diagnostics: &mut Vec<RuleDiagnostic>) -> Option<LoadedRule> {
    let within = candidate.is_global || root.is_some_and(|root| {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Path::new(&candidate.real_path).strip_prefix(root).is_ok_and(|relative| !relative.to_string_lossy().starts_with(".."))
    });
    if !within { diagnostics.push(RuleDiagnostic { severity: "warning".into(), source: candidate.path.clone(), message: "Rule file resolves outside project root".into() }); return None; }
    let content = match std::fs::read_to_string(&candidate.path) {
        Ok(content) => content,
        Err(_) => { diagnostics.push(RuleDiagnostic { severity: "warning".into(), source: candidate.path.clone(), message: "Unable to read rule file".into() }); return None; },
    };
    let parsed = parse_rule(&content);
    if let Some(message) = parsed.diagnostic { diagnostics.push(RuleDiagnostic { severity: "warning".into(), source: candidate.path.clone(), message }); }
    Some(LoadedRule { candidate, frontmatter: parsed.frontmatter, body: parsed.body, content_hash: hash_content(&content), match_reason: MatchReason::NoMatch })
}
