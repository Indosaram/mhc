use std::collections::BTreeMap;
use crate::{rule_condition::compile_rule_condition,scope::{has_reachable_scope,matches_path_globs,matches_scope},types::*};
pub struct TtsrMatchContext { pub source:TtsrStreamSource,pub stream_key:String,pub tool_name:Option<String>,pub file_paths:Option<Vec<String>> }
struct Entry { rule:TtsrRule,conditions:Vec<fancy_regex::Regex> }
pub struct TtsrManager { settings:TtsrSettings,rules:Vec<Entry>,injection_records:Vec<(String,u64)>,buffers:BTreeMap<String,Vec<u16>>,max_condition_length:usize,message_count:u64,can_match_text:bool,can_match_thinking:bool }
impl TtsrManager {
    pub fn new(settings:TtsrSettings)->Self { Self { settings,rules:vec![],injection_records:vec![],buffers:BTreeMap::new(),max_condition_length:0,message_count:0,can_match_text:false,can_match_thinking:false } }
    pub fn add_rule(&mut self,mut rule:TtsrRule)->bool {
        if !self.settings.enabled || self.settings.disabled_rules.contains(&rule.name) || self.rules.iter().any(|entry|entry.rule.name==rule.name) { return false; }
        let mut conditions=Vec::new();
        for pattern in &rule.condition { if let Some(regex)=compile_rule_condition(pattern).regex { conditions.push(regex); self.max_condition_length=self.max_condition_length.max(pattern.encode_utf16().count()); } }
        if conditions.is_empty() || !has_reachable_scope(&rule.scope) { return false; }
        if let Some(globs)=&mut rule.globs { globs.retain(|glob|globset::GlobBuilder::new(glob).literal_separator(true).build().is_ok()); }
        for tool in &mut rule.scope.tool_scopes { if tool.path_glob.as_ref().is_some_and(|glob|globset::GlobBuilder::new(glob).literal_separator(true).build().is_err()) { tool.path_glob=None; } }
        self.can_match_text|=rule.scope.allow_text; self.can_match_thinking|=rule.scope.allow_thinking;
        self.rules.push(Entry { rule,conditions }); true
    }
    pub fn check_delta(&mut self,delta:&str,context:&TtsrMatchContext)->Vec<TtsrRule> {
        if (context.source==TtsrStreamSource::Text && !self.can_match_text) || (context.source==TtsrStreamSource::Thinking && !self.can_match_thinking) { return vec![]; }
        let source=match context.source { TtsrStreamSource::Text=>"text",TtsrStreamSource::Thinking=>"thinking",TtsrStreamSource::Tool=>"tool" };
        let cap=1024.max(self.max_condition_length.saturating_mul(4));
        let buffer=self.buffers.entry(format!("{source}:{}",context.stream_key)).or_default(); buffer.extend(delta.encode_utf16());
        if buffer.len()>cap { buffer.drain(..buffer.len()-cap); }
        let buffer=String::from_utf16_lossy(buffer);
        if !self.settings.enabled { return vec![]; }
        self.rules.iter().filter(|entry| {
            let eligible=self.injection_records.iter().find(|(name,_)|name==&entry.rule.name).is_none_or(|(_,last)|self.settings.repeat_mode!=RepeatMode::Once && self.message_count.saturating_sub(*last)>=self.settings.repeat_gap);
            eligible && matches_scope(&entry.rule.scope,context.source,context.tool_name.as_deref(),context.file_paths.as_deref()) && matches_path_globs(entry.rule.globs.as_deref().unwrap_or(&[]),context.file_paths.as_deref()) && entry.conditions.iter().any(|condition|condition.is_match(&buffer).unwrap_or(false))
        }).map(|entry|entry.rule.clone()).collect()
    }
    pub fn stream_buffer_lengths(&self)->BTreeMap<String,usize> { self.buffers.iter().map(|(key,value)|(key.clone(),value.len())).collect() }
    fn record(&mut self,name:&str,at:u64) { if let Some((_,last))=self.injection_records.iter_mut().find(|(key,_)|key==name) { *last=at; } else { self.injection_records.push((name.into(),at)); } }
    pub fn mark_injected(&mut self,rules:&[TtsrRule]) { for rule in rules { self.mark_injected_by_names(std::slice::from_ref(&rule.name)); } }
    pub fn mark_injected_by_names(&mut self,names:&[String]) { for name in names { let name=name.trim(); if !name.is_empty() { self.record(name,self.message_count); } } }
    pub fn injected_rule_names(&self)->Vec<String> { self.injection_records.iter().map(|(name,_)|name.clone()).collect() }
    pub fn restore_injected(&mut self,names:&[String]) { for name in names { self.record(name,0); } }
    pub fn reset_buffers(&mut self) { self.buffers.clear(); }
    pub fn has_rules(&self)->bool { self.settings.enabled && !self.rules.is_empty() }
    pub fn rules(&self)->Vec<TtsrRule> { self.rules.iter().map(|entry|entry.rule.clone()).collect() }
    pub fn increment_message_count(&mut self) { self.message_count+=1; }
    pub fn message_count(&self)->u64 { self.message_count }
    pub fn settings(&self)->&TtsrSettings { &self.settings }
}
#[cfg(test)] mod tests {
    use super::*;
    fn rule(name:&str,condition:&str)->TtsrRule { TtsrRule { name:name.into(),path:None,content:String::new(),description:None,globs:None,condition:vec![condition.into()],scope:crate::scope::parse_scope(&[]),interrupt_mode:TtsrInterruptMode::Always,source:RuleSource::Project } }
    fn context(key:&str)->TtsrMatchContext { TtsrMatchContext { source:TtsrStreamSource::Text,stream_key:key.into(),tool_name:None,file_paths:None } }
    #[test] fn delta_matches_across_chunks_and_preserves_rule_order() { let mut manager=TtsrManager::new(TtsrSettings::default()); manager.add_rule(rule("second","needle")); manager.add_rule(rule("first","needle")); assert!(manager.check_delta("nee",&context("a")).is_empty()); let names=manager.check_delta("dle",&context("a")).into_iter().map(|rule|rule.name).collect::<Vec<_>>(); assert_eq!(names,["second","first"]); }
    #[test] fn streams_are_isolated() { let mut manager=TtsrManager::new(TtsrSettings::default()); manager.add_rule(rule("test","needle")); manager.check_delta("nee",&context("a")); let result=manager.check_delta("dle",&context("b")); assert!(result.is_empty()); }
    #[test] fn once_injection_suppresses_future_matches() { let mut manager=TtsrManager::new(TtsrSettings::default()); manager.add_rule(rule("test","x")); manager.mark_injected_by_names(&["test".into()]); manager.increment_message_count(); let result=manager.check_delta("x",&context("a")); assert!(result.is_empty()); }
    #[test] fn repeat_gap_uses_message_count() { let mut manager=TtsrManager::new(TtsrSettings { repeat_mode:RepeatMode::AfterGap,repeat_gap:2,..Default::default() }); manager.add_rule(rule("test","x")); manager.mark_injected_by_names(&["test".into()]); manager.increment_message_count(); assert!(manager.check_delta("x",&context("a")).is_empty()); manager.increment_message_count(); let result=manager.check_delta("x",&context("a")); assert_eq!(result.len(),1); }
    #[test] fn buffers_are_bounded_and_reset() { let mut manager=TtsrManager::new(TtsrSettings::default()); manager.add_rule(rule("test","x")); manager.check_delta(&"x".repeat(10000),&context("a")); assert_eq!(manager.stream_buffer_lengths()["text:a"],1024); manager.reset_buffers(); assert!(manager.stream_buffer_lengths().is_empty()); }
    #[test] fn invalid_and_disabled_rules_are_not_added() { let mut manager=TtsrManager::new(TtsrSettings { disabled_rules:vec!["disabled".into()],..Default::default() }); assert!(!manager.add_rule(rule("disabled","x"))); assert!(!manager.add_rule(rule("invalid","("))); assert!(!manager.has_rules()); }
    #[test] fn restore_preserves_injection_name_order_and_empty_names() { let mut manager=TtsrManager::new(TtsrSettings::default()); manager.restore_injected(&["b".into(),"".into(),"a".into()]); manager.mark_injected_by_names(&["b".into()]); assert_eq!(manager.injected_rule_names(),["b","","a"]); }
}
