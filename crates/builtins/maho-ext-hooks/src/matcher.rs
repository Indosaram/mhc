use crate::diagnostics::{DiagnosticDraft,diagnostic};
use crate::types::{ExecutableHookHandler,HookDiagnostic,Severity,SupportedHookEvent};

pub struct HookMatcherInput<'a> {
    pub event: SupportedHookEvent,
    pub tool_name: &'a str,
}

pub struct HookMatcherResult<'a> {
    pub handlers: Vec<&'a ExecutableHookHandler>,
    pub diagnostics: Vec<HookDiagnostic>,
}

pub fn matching_hook_handlers<'a>(input: HookMatcherInput<'_>, handlers: &'a [ExecutableHookHandler]) -> HookMatcherResult<'a> {
    use SupportedHookEvent as E;
    let subjects = match input.event {
        E::UserPromptSubmit | E::Stop | E::Notification => None,
        E::PreToolUse | E::PostToolUse => Some(tool_matcher_inputs(input.tool_name)),
        E::SessionStart | E::PreCompact | E::PostCompact => Some(vec![input.event.as_str().to_owned()]),
    };
    let mut result=HookMatcherResult {handlers:Vec::new(),diagnostics:Vec::new()};
    for handler in handlers {
        if handler.event != input.event {continue;}
        let Some(subjects)=&subjects else {result.handlers.push(handler);continue;};
        let matcher=handler.matcher.as_deref().unwrap_or("").trim();
        if matcher.is_empty() || matcher=="*" {result.handlers.push(handler);continue;}
        let literal=matcher.split(['|',',']).map(str::trim).filter(|s|!s.is_empty()).any(|s|subjects.iter().any(|input|input==s));
        let matched=match fancy_regex::Regex::new(matcher) {
            Ok(regex)=>literal || subjects.iter().any(|s|regex.is_match(s).unwrap_or(false)),
            Err(error)=> {
                result.diagnostics.push(diagnostic(DiagnosticDraft {
                    code:"invalid_matcher",event:Some(handler.event.as_str()),
                    message:format!("Hook matcher is not a valid JavaScript regular expression: {error}"),
                    path:format!("hooks.{}[{}].matcher",handler.event.as_str(),handler.group_index),severity:Some(Severity::Warning),
                },&handler.source));literal
            }
        };
        if matched {result.handlers.push(handler);}
    }
    result
}

fn tool_matcher_inputs(tool_name:&str)->Vec<String> {
    let lower=tool_name.to_lowercase();
    let claude=lower.split(['-','_']).filter(|s|!s.is_empty()).map(|s| {
        let mut chars=s.chars();let mut name=String::new();
        if let Some(first)=chars.next() {name.extend(first.to_uppercase());name.extend(chars);}name
    }).collect::<String>();
    let aliases:&[&str]=match lower.as_str() {
        "apply_patch"=>&["ApplyPatch","functions.apply_patch"],
        "bash"=>&["Bash","Shell","shell","exec_command","functions.exec_command"],
        "create_goal"=>&["CreateGoal"],"edit"=>&["Edit","MultiEdit","multi_edit"],
        "find"=>&["Find","Glob","glob","file_search"],"get_goal"=>&["GetGoal"],
        "grep"=>&["Grep","Search","grep_app"],"ls"=>&["LS","List","list"],
        "read"=>&["Read","open","read_file"],"todo"=>&["Todo"],"todoread"=>&["TodoRead"],
        "todowrite"=>&["TodoWrite"],"update_goal"=>&["UpdateGoal"],
        "web_search"=>&["WebSearch","web-search"],"webfetch"=>&["WebFetch","web_fetch","web-fetch"],
        "write"=>&["Write","write_file"],_=>&[],
    };
    let mut values=Vec::new();
    for value in [tool_name,lower.as_str(),claude.as_str()].into_iter().chain(aliases.iter().copied()) {
        let value=value.trim();if !value.is_empty() && !values.iter().any(|v|v==value) {values.push(value.to_owned());}
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_hook_config;
    use crate::types::{HookDiscoveryTiming,HookSourceMetadata,HookSourceScope};
    use serde_json::json;

    fn handlers(event:SupportedHookEvent,matchers:&[Option<&str>])->Vec<ExecutableHookHandler> {
        let hooks:Vec<_>=matchers.iter().map(|matcher| {
            let mut group=json!({"hooks":[{"type":"command","command":"hook"}]});
            if let Some(value)=matcher {group["matcher"]=json!(value);}group
        }).collect();
        parse_hook_config(&json!({"hooks":{event.as_str():hooks}}),&HookSourceMetadata {
            scope:HookSourceScope::Project,source_path:"/repo/.senpi/hooks.json".to_owned(),display_order:11,
            discovered_at:HookDiscoveryTiming::PreSession,plugin_root:None,manifest_path:None,plugin_env:None,
        }).executable_handlers
    }

    #[test]
    fn exact_lists_regex_aliases_match_once() {
        let handlers=handlers(SupportedHookEvent::PreToolUse,&[None,Some(""),Some("*"),Some("bash"),Some("Bash"),Some("shell"),Some("exec_command"),Some("Read|Edit|Bash"),Some("Read, Edit, Bash"),Some("^(Read|Bash)$"),Some("bash|Bash|shell|exec_command"),Some("Read")]);
        let result=matching_hook_handlers(HookMatcherInput {event:SupportedHookEvent::PreToolUse,tool_name:"bash"},&handlers);
        assert!(result.diagnostics.is_empty());assert_eq!(result.handlers.len(),11);
        assert_eq!(result.handlers.iter().map(|h|h.group_index).collect::<Vec<_>>(),(0..11).collect::<Vec<_>>());
    }

    #[test]
    fn invalid_regex_falls_back_to_literal_lists() {
        let handlers=handlers(SupportedHookEvent::PreToolUse,&[Some("Bash, malformed_input("),Some("malformed_input(")]);
        let result=matching_hook_handlers(HookMatcherInput {event:SupportedHookEvent::PreToolUse,tool_name:"bash"},&handlers);
        assert_eq!(result.handlers.len(),1);assert_eq!(result.handlers[0].group_index,0);assert_eq!(result.diagnostics.len(),2);
        assert!(result.diagnostics.iter().all(|d|d.code=="invalid_matcher"));
    }

    #[test]
    fn prompt_and_stop_ignore_matcher() {
        for event in [SupportedHookEvent::UserPromptSubmit,SupportedHookEvent::Stop] {
            let handlers=handlers(event,&[Some("malformed_input(")]);
            let result=matching_hook_handlers(HookMatcherInput {event,tool_name:""},&handlers);
            assert!(result.diagnostics.is_empty());assert_eq!(result.handlers.len(),1);
        }
    }
}
