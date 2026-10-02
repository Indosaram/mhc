use std::{collections::BTreeSet,sync::OnceLock};
use crate::types::{TtsrScope,TtsrStreamSource,TtsrToolScope};
pub const ANY_TOOL_NAME:&str="*";
pub fn parse_scope(tokens:&[String])->TtsrScope {
    if tokens.is_empty() { return TtsrScope { allow_text:true,allow_thinking:false,tool_scopes:vec![TtsrToolScope { tool_name:ANY_TOOL_NAME.into(),path_glob:None }] }; }
    static TOKEN:OnceLock<regex::Regex>=OnceLock::new();
    let pattern=TOKEN.get_or_init(||match regex::Regex::new(r"(?i)\A(?:(?P<prefix>tool)(?::(?P<tool>[a-z0-9_-]+))?|(?P<bare>[a-z0-9_-]+))(?:\((?P<path>[^)]+)\))?\z") { Ok(regex)=>regex,Err(error)=>panic!("invalid static scope expression: {error}") });
    let mut scope=TtsrScope { allow_text:false,allow_thinking:false,tool_scopes:vec![] }; let mut seen=BTreeSet::new();
    for raw in tokens {
        let token=maho_ai::utils::js::trim(raw); if token.is_empty() { continue; }
        let normalized=token.to_lowercase();
        match normalized.as_str() { "text"=>{scope.allow_text=true;continue;},"thinking"=>{scope.allow_thinking=true;continue;},_=>{} }
        let tool=if matches!(normalized.as_str(),"tool"|"toolcall") { Some(TtsrToolScope { tool_name:ANY_TOOL_NAME.into(),path_glob:None }) } else { pattern.captures(token).map(|groups|TtsrToolScope { tool_name:groups.name("tool").or_else(||groups.name("bare")).map_or_else(||ANY_TOOL_NAME.into(),|name|name.as_str().to_lowercase()),path_glob:groups.name("path").map(|path|maho_ai::utils::js::trim(path.as_str())).filter(|path|!path.is_empty()).map(str::to_owned) }) };
        if let Some(tool)=tool && seen.insert(format!("{}({})",tool.tool_name,tool.path_glob.as_deref().unwrap_or(""))) { scope.tool_scopes.push(tool); }
    }
    scope
}
pub fn has_reachable_scope(scope:&TtsrScope)->bool { scope.allow_text || scope.allow_thinking || !scope.tool_scopes.is_empty() }
fn matches_any_path(pattern:&str,paths:Option<&[String]>)->bool {
    let Ok(glob)=globset::GlobBuilder::new(pattern).literal_separator(true).build() else { return false; }; let matcher=glob.compile_matcher();
    paths.is_some_and(|paths|paths.iter().any(|path| { let normalized=path.replace('\\',"/"); matcher.is_match(&normalized) || normalized.rsplit_once('/').is_some_and(|(_,basename)|matcher.is_match(basename)) }))
}
pub fn matches_scope(scope:&TtsrScope,source:TtsrStreamSource,tool_name:Option<&str>,paths:Option<&[String]>)->bool {
    match source { TtsrStreamSource::Text=>scope.allow_text,TtsrStreamSource::Thinking=>scope.allow_thinking,TtsrStreamSource::Tool=>{ let name=tool_name.map(|name|maho_ai::utils::js::trim(name).to_lowercase()); scope.tool_scopes.iter().any(|tool| (tool.tool_name==ANY_TOOL_NAME || Some(tool.tool_name.to_lowercase())==name) && tool.path_glob.as_ref().is_none_or(|glob|matches_any_path(glob,paths))) } }
}
pub fn matches_path_globs(globs:&[String],paths:Option<&[String]>)->bool { globs.is_empty() || globs.iter().any(|glob|matches_any_path(glob,paths)) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn default_allows_text_and_any_tool() { let scope=parse_scope(&[]); let result=(matches_scope(&scope,TtsrStreamSource::Text,None,None),matches_scope(&scope,TtsrStreamSource::Tool,Some("edit"),None),matches_scope(&scope,TtsrStreamSource::Thinking,None,None)); assert_eq!(result,(true,true,false)); }
    #[test] fn scope_deduplicates_tools() { let scope=parse_scope(&["tool:edit(*.rs)".into(),"EDIT(*.rs)".into(),"thinking".into()]); assert_eq!(scope.tool_scopes.len(),1); assert!(scope.allow_thinking); }
    #[test] fn basename_matches_and_backslashes_normalize() { let scope=parse_scope(&["tool:edit(*.rs)".into()]); let result=matches_scope(&scope,TtsrStreamSource::Tool,Some("EDIT"),Some(&["C:\\src\\lib.rs".into()])); assert!(result); }
    #[test] fn scoped_glob_requires_paths() { let scope=parse_scope(&["tool:edit(*.rs)".into()]); let result=matches_scope(&scope,TtsrStreamSource::Tool,Some("edit"),None); assert!(!result); }
    #[test] fn invalid_tokens_leave_unreachable_scope() { let scope=parse_scope(&["???".into()]); let result=has_reachable_scope(&scope); assert!(!result); }
    #[test] fn empty_path_globs_match_without_paths() { let result=matches_path_globs(&[],None); assert!(result); }
    #[test] fn dotfiles_are_matched() { let result=matches_path_globs(&["*.rs".into()],Some(&[".hidden.rs".into()])); assert!(result); }
    #[test] fn scope_and_tool_names_trim_ecmascript_bom_not_next_line() {
        let scope=parse_scope(&["\u{feff}text\u{feff}".into(),"\u{feff}tool:edit(\u{feff}*.rs\u{feff})\u{feff}".into()]);
        assert!(scope.allow_text); assert_eq!(scope.tool_scopes[0].path_glob.as_deref(),Some("*.rs"));
        assert!(matches_scope(&scope,TtsrStreamSource::Tool,Some("\u{feff}EDIT\u{feff}"),Some(&["src/lib.rs".into()])));
        assert!(!has_reachable_scope(&parse_scope(&["\u{0085}text\u{0085}".into()])));
    }
}
