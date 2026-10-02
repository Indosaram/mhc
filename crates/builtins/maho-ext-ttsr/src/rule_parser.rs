use serde_json::{Map,Value};
use crate::{rule_condition::compile_rule_condition,scope::{has_reachable_scope,parse_scope},types::*};
pub struct RuleFileMeta { pub name:String,pub path:Option<String>,pub source:RuleSource }
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct SkippedRule { pub name:String,pub warning:String }
fn fields(metadata:&str)->Map<String,Value> {
    if let Ok(value)=serde_yaml::from_str::<Value>(metadata) { return value.as_object().cloned().unwrap_or_default(); }
    let mut fields=Map::new();
    for line in metadata.split('\n') { let Some((key,raw))=line.split_once(':') else { continue; }; if key.is_empty() || !key.chars().all(|c|c.is_ascii_alphanumeric() || matches!(c,'_'|'-')) { continue; } let raw=maho_ai::utils::js::trim(raw); let parsed=serde_yaml::from_str::<Value>(raw).ok().filter(|value|!value.is_object()).unwrap_or_else(||Value::String(raw.into())); fields.insert(key.into(),if raw.is_empty() { Value::String(String::new()) } else { parsed }); }
    fields
}
fn string_list(value:Option<&Value>)->Vec<String> {
    let values=match value { Some(Value::String(text))=>vec![text.as_str()],Some(Value::Array(values))=>values.iter().filter_map(Value::as_str).collect(),_=>vec![] }; let mut result=Vec::new();
    for text in values { let text=maho_ai::utils::js::trim(text); if !text.is_empty() && !result.iter().any(|item|item==text) { result.push(text.into()); } } result
}
fn split_scope(value:&str)->Vec<String> {
    let mut result=Vec::new(); let mut current=String::new(); let mut depth=[0usize;3]; let mut quote=None; let mut previous=None;
    for c in value.chars() {
        if let Some(q)=quote { current.push(c); if c==q && previous!=Some('\\') { quote=None; } previous=Some(c); continue; }
        match c { '\''|'"'=>quote=Some(c),'('=>depth[0]+=1,')'=>depth[0]=depth[0].saturating_sub(1),'['=>depth[1]+=1,']'=>depth[1]=depth[1].saturating_sub(1),'{'=>depth[2]+=1,'}'=>depth[2]=depth[2].saturating_sub(1),',' if depth==[0;3]=>{ result.push(std::mem::take(&mut current)); previous=Some(c); continue; },_=>{} }
        current.push(c); previous=Some(c);
    }
    result.push(current); result.into_iter().filter_map(|token| { let token=maho_ai::utils::js::trim(&token); let token=if token.len()>=2 && (token.starts_with('"') && token.ends_with('"') || token.starts_with('\'') && token.ends_with('\'')) { maho_ai::utils::js::trim(&token[1..token.len()-1]) } else { token }; (!token.is_empty()).then(||token.into()) }).collect()
}
fn file_glob(token:&str)->bool { !token.chars().any(|c|matches!(c,'\\'|'^'|'$'|'+'|'|'|'('|')')) && token.chars().any(|c|matches!(c,'?'|'*'|'['|']'|'{'|'}')) && (token.contains('/') || token.strip_prefix("*.").is_some_and(|suffix|!suffix.is_empty() && !suffix.chars().any(|c|c=='/'||maho_ai::utils::js::is_js_whitespace(c)))) }
pub fn parse_rule_file(markdown:&str,meta:RuleFileMeta)->Result<TtsrRule,SkippedRule> {
    let normalized=markdown.replace("\r\n","\n").replace('\r',"\n");
    let (fields,body)=if let Some(rest)=normalized.strip_prefix("---") { if let Some(end)=rest.find("\n---").map(|end|end+3) { (fields(normalized.get(4..end).unwrap_or("")),maho_ai::utils::js::trim(&normalized[end+4..]).to_owned()) } else { (Map::new(),normalized) } } else { (Map::new(),normalized) };
    let raw=fields.get("condition").filter(|value|!value.is_null()).or_else(||fields.get("ttsr_trigger").filter(|value|!value.is_null())).or_else(||fields.get("ttsrTrigger"));
    let mut conditions=Vec::new(); let mut scopes=string_list(fields.get("scope")).iter().flat_map(|value|split_scope(value)).collect::<Vec<_>>(); let mut inferred=false;
    for token in string_list(raw) { if file_glob(&token) { inferred=true; for tool in ["edit","write"] { scopes.push(format!("tool:{tool}({token})")); } } else { conditions.push(token); } }
    if conditions.is_empty() && inferred { conditions.push(".*".into()); }
    if conditions.is_empty() { return Err(SkippedRule { warning:format!("rule \"{}\" has no condition or legacy ttsr_trigger alias, skipping",meta.name),name:meta.name }); }
    for pattern in &conditions { let compiled=compile_rule_condition(pattern); if compiled.regex.is_none() { return Err(SkippedRule { warning:format!("rule \"{}\" skipped: {}",meta.name,compiled.warning.unwrap_or_else(||"invalid condition".into())),name:meta.name }); } }
    let scope=parse_scope(&scopes);
    if !has_reachable_scope(&scope) { return Err(SkippedRule { warning:format!("rule \"{}\" scope excludes every stream, skipping",meta.name),name:meta.name }); }
    let globs=string_list(fields.get("globs"));
    Ok(TtsrRule { name:meta.name,path:meta.path,source:meta.source,content:body,description:fields.get("description").and_then(Value::as_str).map(str::to_owned),globs:(!globs.is_empty()).then_some(globs),condition:conditions,scope,interrupt_mode:if fields.get("interruptMode").and_then(Value::as_str)==Some("never") { TtsrInterruptMode::Never } else { TtsrInterruptMode::Always } })
}
#[cfg(test)] mod tests {
    use super::*;
    fn meta()->RuleFileMeta { RuleFileMeta { name:"test".into(),path:None,source:RuleSource::Project } }
    #[test] fn fallback_metadata_values_trim_ecmascript_bom() {
        let rule=parse_rule_file("---\ncondition: bad\nscope: text\ninterruptMode: \u{feff}never\u{feff}\nbroken: [\n---\nbody",meta()).unwrap(); assert_eq!(rule.interrupt_mode,TtsrInterruptMode::Never);
    }
    #[test] fn extension_glob_inference_uses_ecmascript_whitespace() {
        assert!(!file_glob("*.a\u{feff}b")); assert!(file_glob("*.a\u{0085}b"));
        let rule=parse_rule_file("---\ncondition: '*.a\u{feff}b'\n---\nbody",meta()); assert!(rule.is_err());
    }
    #[test] fn upstream_malformed_scope_reporter_preserves_matching_and_metadata() {
        let rule=parse_rule_file("---\nname: fix-failures-now\ndescription: prohibits pre-existing classification.\ncondition: \"(?i)(pre.existing|also fails on master|check.*master.*first)\"\nscope: \"text\",\"thinking\"\n---\nbody",RuleFileMeta { name:"fix-failures-now".into(),path:Some("rules/fix-failures-now.md".into()),source:RuleSource::Project }).unwrap();
        assert!(rule.scope.allow_text); assert!(rule.scope.allow_thinking); assert!(rule.scope.tool_scopes.is_empty());
        let regex=compile_rule_condition(&rule.condition[0]).regex.unwrap(); assert!(regex.find("The CI failure was 4 pre-existing GPS map VR mismatches").is_some()); assert!(regex.find("Everything also fails on master anyway").is_some());
        assert_eq!(rule.description.as_deref(),Some("prohibits pre-existing classification.")); assert_eq!(rule.content,"body"); assert_eq!(rule.path.as_deref(),Some("rules/fix-failures-now.md")); assert_eq!(rule.interrupt_mode,TtsrInterruptMode::Always);
    }
    #[test] fn upstream_absent_scope_defaults_to_text_and_any_tool() {
        let rule=parse_rule_file("---\ncondition: bad\n---\nbody",meta()).unwrap(); assert!(rule.scope.allow_text); assert!(!rule.scope.allow_thinking); assert_eq!(rule.scope.tool_scopes[0].tool_name,"*");
    }
    #[test] fn upstream_globs_and_never_interrupt_metadata_survive() {
        let rule=parse_rule_file("---\ncondition: bad\nglobs: ['*.rs', 'src/**']\ninterruptMode: never\n---\nbody",meta()).unwrap(); assert_eq!(rule.globs.unwrap(),["*.rs","src/**"]); assert_eq!(rule.interrupt_mode,TtsrInterruptMode::Never);
    }
    #[test] fn upstream_unreachable_scope_rejects_parsed_rule() { assert!(parse_rule_file("---\ncondition: bad\nscope: '???'\n---\nbody",meta()).is_err()); }
    #[test] fn yaml_frontmatter_preserves_body_and_scope() { let rule=parse_rule_file("---\ncondition: bad\nscope: thinking\ninterruptMode: never\n---\n  body  ",meta()).unwrap(); assert_eq!(rule.content,"body"); assert!(rule.scope.allow_thinking); assert!(!rule.scope.allow_text); assert_eq!(rule.interrupt_mode,TtsrInterruptMode::Never); }
    #[test] fn extension_glob_infers_edit_and_write() { let rule=parse_rule_file("---\ncondition: '*.rs'\n---\nbody",meta()).unwrap(); assert_eq!(rule.condition,[".*"]); assert_eq!(rule.scope.tool_scopes.len(),2); assert!(!rule.scope.allow_text); }
    #[test] fn aliases_and_null_condition_are_supported() { let rule=parse_rule_file("---\ncondition: null\nttsrTrigger: bad\n---\nbody",meta()).unwrap(); assert_eq!(rule.condition,["bad"]); }
    #[test] fn malformed_yaml_falls_back_to_individual_fields() { let rule=parse_rule_file("---\ncondition: bad\nbroken: [\n---\nbody",meta()).unwrap(); assert_eq!(rule.condition,["bad"]); }
    #[test] fn scope_commas_inside_braces_do_not_split() { let rule=parse_rule_file("---\ncondition: bad\nscope: 'tool:edit(*.{rs,ts}), text'\n---\nbody",meta()).unwrap(); assert_eq!(rule.scope.tool_scopes[0].path_glob.as_deref(),Some("*.{rs,ts}")); assert!(rule.scope.allow_text); }
    #[test] fn invalid_regex_rejects_whole_rule() { let result=parse_rule_file("---\ncondition: ['ok', '(']\n---\nbody",meta()); assert!(result.is_err()); }
    #[test] fn missing_condition_rejects_rule() { let result=parse_rule_file("body",meta()); assert!(result.is_err()); }
    #[test] fn condition_and_scope_lists_trim_javascript_bom() {
        let rule=parse_rule_file("---\ncondition: '\u{feff}bad\u{feff}'\nscope: '\u{feff}text\u{feff}'\n---\nbody",meta()).unwrap(); assert_eq!(rule.condition,["bad"]); assert!(rule.scope.allow_text);
    }
}
