use std::{collections::{BTreeMap,BTreeSet,VecDeque},path::{Path,PathBuf}};
use super::{cache::{self,SessionState},constants::{DEFAULT_MAX_RESULT_CHARS,DEFAULT_MAX_RULE_CHARS,PROJECT_SINGLE_FILES,SOURCE_PRIORITY},finder::FinderOptions,formatter::{self,FormatOptions},matcher::hash_content,ordering::compare_candidates,parser::parse_rule,tool_paths::resolve_path,types::*};

pub trait EngineDeps {
    fn find_candidates(&mut self,options:FinderOptions<'_>)->Vec<RuleCandidate>;
    fn read_file(&mut self,path:&str)->Option<String>;
    fn find_project_root(&mut self,path:&str)->Option<String>;
    fn file_fingerprint(&mut self,path:&str)->String{
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).map_or_else(|_|"missing".into(),|stats|format!("{}:{}:{}",i128::from(stats.mtime())*1_000_000_000+i128::from(stats.mtime_nsec()),i128::from(stats.ctime())*1_000_000_000+i128::from(stats.ctime_nsec()),stats.len()))
    }
    fn match_rule(&mut self,matcher:&mut super::matcher::Matcher,input:super::matcher::MatcherInput<'_>)->Result<super::matcher::MatchResult,globset::Error>{matcher.match_rule(input)}
}
#[derive(Default,Debug,PartialEq,Eq)]
pub struct LoadResult{pub rules:Vec<LoadedRule>,pub diagnostics:Vec<RuleDiagnostic>}
pub struct Engine<D>{pub state:SessionState,pub config:PiRulesConfig,pub deps:D,dynamic_matches:VecDeque<(String,Option<MatchReason>)>}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct DynamicTargetFingerprint{pub target_path:String,pub cache_key:String,pub fingerprint:String}
pub fn default_config()->PiRulesConfig{PiRulesConfig{disabled:false,mode:Mode::Both,max_rule_chars:DEFAULT_MAX_RULE_CHARS,max_result_chars:DEFAULT_MAX_RESULT_CHARS,enabled_sources:EnabledSources::Auto}}
impl<D:EngineDeps> Engine<D>{
    pub fn new(config:PiRulesConfig,deps:D)->Self{Self{state:cache::create_session_state(None),config,deps,dynamic_matches:VecDeque::new()}}
    pub fn load_static_rules(&mut self,cwd:&str)->LoadResult{
        self.state.cwd=Some(cwd.into());
        let mut result=LoadResult::default();
        if !self.config.disabled&&!matches!(self.config.mode,Mode::Off|Mode::Dynamic){
            let root=self.deps.find_project_root(cwd);
            let mut real_paths=BTreeMap::new();
            let disabled=disabled_sources_for(&self.config);
            let mut candidates=self.deps.find_candidates(FinderOptions{project_root:root.as_deref().map(Path::new),target_file:None,home_dir:None,disabled_sources:disabled.as_ref(),skip_user_home:false,cache:None});
            candidates.sort_by(compare_candidates);
            let mut root_single_selected=false;
            for candidate in candidates{
                if root_single_selected&&is_root_single_file(&candidate){continue;}
                let Some(mut rule)=load_candidate(candidate,&mut self.deps,&mut result.diagnostics,root.as_deref(),&mut real_paths)else{continue;};
                let reason=if rule.frontmatter.always_apply==Some(true){MatchReason::AlwaysApply}else if rule.candidate.is_single_file{MatchReason::SingleFile}else{continue;};
                if is_root_single_file(&rule.candidate){root_single_selected=true;}
                rule.match_reason=reason;result.rules.push(rule);
            }
            result.rules.sort_by(|a,b|compare_candidates(&a.candidate,&b.candidate));
        }
        self.state.loaded_rules=result.rules.clone();self.state.diagnostics=result.diagnostics.clone();result
    }
    pub fn format_static(&self,rules:&[LoadedRule])->String{formatter::format_static_block(rules,&self.format_options())}
    pub fn load_dynamic_rules(&mut self,cwd:&str,targets:&[String])->Result<LoadResult,globset::Error>{
        self.state.cwd=Some(cwd.into());
        let mut result=LoadResult::default();
        if !self.config.disabled&&!matches!(self.config.mode,Mode::Off|Mode::Static){
            let mut matcher=super::matcher::Matcher::default();
            let mut real_paths=BTreeMap::new();
            let disabled=disabled_sources_for(&self.config);
            let mut seen_targets=BTreeSet::new();let mut seen_rules=BTreeSet::new();let mut selected_roots=BTreeSet::new();
            let mut loaded:BTreeMap<String,Option<LoadedRule>>=BTreeMap::new();
            let mut cached_diagnostics:BTreeMap<String,Vec<RuleDiagnostic>>=BTreeMap::new();
            let cache_lookups=targets.iter().collect::<BTreeSet<_>>().len()>1;
            let mut roots:BTreeMap<PathBuf,Option<String>>=BTreeMap::new();
            let mut discoveries:BTreeMap<(Option<String>,PathBuf),Vec<RuleCandidate>>=BTreeMap::new();
            let mut discovery_cache=super::finder::RuleDiscoveryCache::default();
            for target in targets{
                if !seen_targets.insert(target){continue;}
                let directory=absolute(target).parent().unwrap_or(Path::new("/")).to_owned();
                let root=if cache_lookups{roots.entry(directory.clone()).or_insert_with(||self.deps.find_project_root(target)).clone()}else{self.deps.find_project_root(target)};
                let key=(root.clone(),directory);
                let candidates=if cache_lookups&&discoveries.contains_key(&key){discoveries[&key].clone()}else{
                    let mut candidates=self.deps.find_candidates(FinderOptions{project_root:root.as_deref().map(Path::new),target_file:Some(Path::new(target)),home_dir:None,disabled_sources:disabled.as_ref(),skip_user_home:false,cache:cache_lookups.then_some(&mut discovery_cache)});
                    candidates.sort_by(compare_candidates);if cache_lookups{discoveries.insert(key,candidates.clone());}candidates
                };
                for candidate in candidates{
                    let root_single=is_root_single_file(&candidate)&&root.is_some();
                    if root_single&&selected_roots.contains(&root){continue;}
                    if !candidate_within_project(&candidate,root.as_deref(),&mut real_paths){result.diagnostics.push(RuleDiagnostic{severity:Severity::Warning,source:candidate.path,message:"Rule file resolves outside project root".into()});continue;}
                    let mut rule=if let Some(cached)=loaded.get(&candidate.real_path){
                        if let Some(diagnostics)=cached_diagnostics.get(&candidate.real_path){result.diagnostics.extend(diagnostics.iter().cloned().map(|mut diagnostic|{diagnostic.source=candidate.path.clone();diagnostic}));}
                        let Some(cached)=cached else{result.diagnostics.push(RuleDiagnostic{severity:Severity::Warning,source:candidate.path,message:"Unable to read rule file".into()});continue;};
                        let mut rule=cached.clone();rule.candidate=candidate;rule
                    }else{
                        let before=result.diagnostics.len();let rule=load_candidate(candidate.clone(),&mut self.deps,&mut result.diagnostics,root.as_deref(),&mut real_paths);if rule.is_some(){cached_diagnostics.insert(candidate.real_path.clone(),result.diagnostics[before..].to_vec());}loaded.insert(candidate.real_path,rule.clone());let Some(rule)=rule else{continue;};rule
                    };
                    let basename=Path::new(target).file_name().unwrap_or_default().to_string_lossy();
                    let project_relative=root.as_ref().map_or_else(||basename.to_string(),|root|relative_path(Path::new(root),Path::new(target)));
                    let scope=if rule.candidate.is_global{None}else if rule.candidate.is_single_file{Path::new(&rule.candidate.path).parent().map(Path::to_owned)}else{root.as_ref().map(|root|{
                        let prefix=rule.candidate.relative_path.find(&rule.candidate.source).map_or("",|index|&rule.candidate.relative_path[..index]);Path::new(root).join(prefix.trim_end_matches('/'))
                    })};
                    let scope_relative=scope.map(|scope|relative_path(&scope,Path::new(target)));
                    let key=[root.as_deref().unwrap_or(""),&absolute(target).to_string_lossy(),&rule.candidate.real_path,&rule.candidate.relative_path,&rule.candidate.source,if rule.candidate.is_global{"global"}else{"project"},if rule.candidate.is_single_file{"single"}else{"multi"},&rule.candidate.distance.to_string(),&rule.content_hash].join("\0");
                    let reason=if let Some(index)=self.dynamic_matches.iter().position(|(cached,_)|cached==&key){
                        let entry=self.dynamic_matches.remove(index).unwrap_or_else(||unreachable!("existing cache index"));let reason=entry.1.clone();self.dynamic_matches.push_back(entry);reason
                    }else{
                        let matched=self.deps.match_rule(&mut matcher,super::matcher::MatcherInput{frontmatter:&rule.frontmatter,is_single_file:rule.candidate.is_single_file,project_relative:&project_relative,scope_relative:scope_relative.as_deref(),basename:&basename})?;
                        let reason=matched.matched.then_some(matched.reason);if self.dynamic_matches.len()>=4096{self.dynamic_matches.pop_front();}self.dynamic_matches.push_back((key,reason.clone()));reason
                    };
                    let Some(reason)=reason else{continue;};
                    if root_single{selected_roots.insert(root.clone());}
                    if !seen_rules.insert(format!("{}::{}",rule.candidate.real_path,rule.content_hash)){continue;}
                    rule.match_reason=reason;result.rules.push(rule);
                }
            }
            result.rules.sort_by(|a,b|compare_candidates(&a.candidate,&b.candidate));
        }
        self.state.loaded_rules=result.rules.clone();self.state.diagnostics=result.diagnostics.clone();Ok(result)
    }
    pub fn format_dynamic(&self,rules:&[LoadedRule],target:&str)->String{formatter::format_dynamic_block(rules,target,&self.format_options())}
    pub fn fingerprint_dynamic_targets(&mut self,cwd:&str,targets:&[String])->Vec<DynamicTargetFingerprint>{
        self.state.cwd=Some(cwd.into());
        if self.config.disabled||matches!(self.config.mode,Mode::Off|Mode::Static)||targets.is_empty(){return Vec::new();}
        let disabled=disabled_sources_for(&self.config);let mut discovery=super::finder::RuleDiscoveryCache::default();let cwd_root=self.deps.find_project_root(cwd);let mut seen=BTreeSet::new();let mut result=Vec::new();
        for target in targets{
            if !seen.insert(target){continue;}
            let root=if cwd_root.as_ref().is_some_and(|root|absolute(target).strip_prefix(absolute(root)).is_ok_and(|relative|!relative.to_string_lossy().starts_with(".."))){cwd_root.clone()}else{self.deps.find_project_root(target)};
            let mut candidates=self.deps.find_candidates(FinderOptions{project_root:root.as_deref().map(Path::new),target_file:Some(Path::new(target)),home_dir:None,disabled_sources:disabled.as_ref(),skip_user_home:false,cache:Some(&mut discovery)});candidates.sort_by(compare_candidates);
            let candidate_fingerprints=candidates.iter().map(|candidate|[candidate.real_path.clone(),candidate.relative_path.clone(),candidate.source.clone(),if candidate.is_global{"global"}else{"project"}.into(),if candidate.is_single_file{"single"}else{"multi"}.into(),candidate.distance.to_string(),self.deps.file_fingerprint(&candidate.path)].join("\0")).collect::<Vec<_>>().join("\u{1}");
            let cache_key=absolute(target).to_string_lossy().replace('\\',"/");let enabled=match &self.config.enabled_sources{EnabledSources::Auto=>"auto".into(),EnabledSources::Sources(sources)=>sources.join(",")};
            let fingerprint=hash_content(&["v1",&enabled,root.as_deref().unwrap_or(""),&cache_key,&candidate_fingerprints].join("\0"));result.push(DynamicTargetFingerprint{target_path:target.clone(),cache_key,fingerprint});
        }result
    }
    pub fn commit_dynamic_target_fingerprints(&mut self,targets:&[DynamicTargetFingerprint]){for target in targets{self.state.dynamic_target_fingerprints.insert(target.cache_key.clone(),target.fingerprint.clone());}}
    pub fn is_dynamic_target_fingerprint_current(&self,target:&DynamicTargetFingerprint)->bool{self.state.dynamic_target_fingerprints.get(&target.cache_key)==Some(&target.fingerprint)}
    fn format_options(&self)->FormatOptions{FormatOptions{max_rule_chars:self.config.max_rule_chars,max_result_chars:self.config.max_result_chars}}
    pub fn reset_session(&mut self,cwd:Option<&str>){cache::clear_session(&mut self.state);self.dynamic_matches.clear();if let Some(cwd)=cwd{self.state.cwd=Some(cwd.into());}}
    pub fn is_static_injected(&self,rule:&LoadedRule)->bool{cache::is_static_injected(&self.state,rule)}
    pub fn is_dynamic_injected(&self,scope:&str,rule:&LoadedRule)->bool{cache::is_dynamic_injected(&self.state,scope,rule)}
    pub fn mark_static_injected(&mut self,rule:&LoadedRule)->bool{cache::mark_static_injected(&mut self.state,rule)}
    pub fn mark_dynamic_injected(&mut self,scope:&str,rule:&LoadedRule)->bool{cache::mark_dynamic_injected(&mut self.state,scope,rule)}
}
fn disabled_sources_for(config:&PiRulesConfig)->Option<BTreeSet<String>>{match &config.enabled_sources{EnabledSources::Auto=>None,EnabledSources::Sources(enabled)=>Some(SOURCE_PRIORITY.iter().filter(|(source,_)|!enabled.iter().any(|s|s==source)).map(|(source,_)|(*source).into()).collect())}}
fn is_root_single_file(candidate:&RuleCandidate)->bool{candidate.is_single_file&&candidate.relative_path==candidate.source&&PROJECT_SINGLE_FILES.iter().any(|source|!source.contains('/')&&*source==candidate.source)}
fn absolute(path:&str)->PathBuf{resolve_path(&std::path::absolute(path).unwrap_or_else(|_|PathBuf::from(path)))}
fn relative_path(base:&Path,target:&Path)->String{
    let base=resolve_path(base);let target=resolve_path(target);let left:Vec<_>=base.components().collect();let right:Vec<_>=target.components().collect();
    let common=left.iter().zip(&right).take_while(|(a,b)|a==b).count();let mut result=PathBuf::new();
    for _ in common..left.len(){result.push("..");}for component in &right[common..]{result.push(component.as_os_str());}result.to_string_lossy().replace('\\',"/")
}
fn candidate_within_project(candidate:&RuleCandidate,root:Option<&str>,real_paths:&mut BTreeMap<String,PathBuf>)->bool{
    candidate.is_global||root.is_some_and(|root|{
        let root=real_paths.entry(root.into()).or_insert_with(||Path::new(root).canonicalize().unwrap_or_else(|_|absolute(root)));
        let real=absolute(&candidate.real_path);
        real.strip_prefix(root).is_ok_and(|relative|!relative.to_string_lossy().starts_with(".."))
    })
}
fn load_candidate<D:EngineDeps>(candidate:RuleCandidate,deps:&mut D,diagnostics:&mut Vec<RuleDiagnostic>,root:Option<&str>,real_paths:&mut BTreeMap<String,PathBuf>)->Option<LoadedRule>{
    if !candidate_within_project(&candidate,root,real_paths){diagnostics.push(RuleDiagnostic{severity:Severity::Warning,source:candidate.path,message:"Rule file resolves outside project root".into()});return None;}
    let Some(content)=deps.read_file(&candidate.path)else{diagnostics.push(RuleDiagnostic{severity:Severity::Warning,source:candidate.path,message:"Unable to read rule file".into()});return None;};
    let parsed=parse_rule(&content);
    if let Some(message)=parsed.diagnostic{diagnostics.push(RuleDiagnostic{severity:Severity::Warning,source:candidate.path.clone(),message});}
    Some(LoadedRule{candidate,frontmatter:parsed.frontmatter,body:parsed.body,content_hash:hash_content(&content),match_reason:MatchReason::NoMatch})
}
