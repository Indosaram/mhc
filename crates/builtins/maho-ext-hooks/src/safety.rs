use std::collections::BTreeMap;
use crate::diagnostics::{DiagnosticDraft,diagnostic};
use crate::plugin_manifest::{is_contained,normalize_path};
use crate::types::{ExecutableHookHandler,HookDiagnostic,HookSourceScope,SupportedHookEvent};
pub use crate::output_bounds::*;
pub const DEFAULT_HOOK_TIMEOUT_SECONDS:f64=600.0;

pub fn is_valid_hook_timeout_seconds(timeout:f64)->bool {timeout.is_finite() && timeout>0.0}
pub fn resolve_hook_timeout_seconds(handler:&ExecutableHookHandler)->Result<f64,&'static str> {
    let timeout=handler.config.timeout.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECONDS);
    if is_valid_hook_timeout_seconds(timeout) {Ok(timeout)} else {Err("Invalid command hook timeout reached runtime execution.")}
}
pub fn build_hook_environment(handler:&ExecutableHookHandler,event:SupportedHookEvent,source_env:&BTreeMap<String,String>,passthrough:&[String])->BTreeMap<String,String> {
    let mut env=BTreeMap::new();
    for key in ["PATH","HOME","USER","USERNAME","LOGNAME","SHELL","TMPDIR","TMP","TEMP","SystemRoot","ComSpec","PATHEXT"].into_iter().chain(passthrough.iter().map(String::as_str)) {if let Some(value)=source_env.get(key) {env.insert(key.to_owned(),value.clone());}}
    if let Some(plugin)=&handler.source.plugin_env {for key in ["PLUGIN_ROOT","PLUGIN_DATA","CLAUDE_PLUGIN_ROOT","CLAUDE_PLUGIN_DATA"] {if let Some(value)=plugin.get(key) {env.insert(key.to_owned(),value.clone());}}}
    env.insert("SENPI_HOOK_SOURCE".to_owned(),handler.source.source_path.clone());env.insert("SENPI_HOOK_EVENT".to_owned(),event.as_str().to_owned());env
}

pub fn validate_hook_handler_safety(handler:&ExecutableHookHandler)->std::io::Result<Vec<HookDiagnostic>> {
    if handler.source.scope!=HookSourceScope::Plugin {return Ok(Vec::new());}
    let Some(root)=handler.source.plugin_root.as_deref() else {return Ok(Vec::new());};let root=normalize_path(&std::path::absolute(root)?);
    let mut diagnostics=Vec::new();
    for (field,command) in [("command",Some(handler.config.command.as_str())),("commandWindows",handler.config.command_windows.as_deref())] {
        let Some(command)=command else {continue;};
        for (raw,expanded) in read_command_words(command) {
            for token in ["${PLUGIN_ROOT}","$PLUGIN_ROOT","%PLUGIN_ROOT%"] {
                if !expanded.contains(token) {continue;}
                let expanded=expanded.replace(token,&root.to_string_lossy()).replace('\\',"/");let path=normalize_path(&std::path::absolute(expanded)?);
                let problem=if !is_contained(&root,&path) {Some(("invalid_command_target",format!("Plugin command target is outside plugin root: {raw}")))}
                else if !path.exists() {Some(("missing_command_target",format!("Plugin command target does not exist: {raw}")))}
                else if !is_contained(&std::fs::canonicalize(&root)?,&std::fs::canonicalize(&path)?) {Some(("invalid_command_target",format!("Plugin command target resolves outside plugin root: {raw}")))} else {None};
                if let Some((code,message))=problem {diagnostics.push(diagnostic(DiagnosticDraft {code,message,path:format!("hooks.{}[{}].hooks[{}].{field}",handler.event.as_str(),handler.group_index,handler.handler_index),event:Some(handler.event.as_str()),severity:None},&handler.source));}
            }
        }
    }Ok(diagnostics)
}

fn read_command_words(command:&str)->Vec<(String,String)> {
    fn terminator(ch:char)->bool {matches!(ch,' '| '\t'|'\n'|';'|'&'|'|'|'<'|'>'|')')}
    let mut chars=command.char_indices().peekable();let mut words=Vec::new();
    while let Some((start,ch))=chars.next() {
        if terminator(ch) {continue;}let mut expanded=String::new();let mut end=start;let mut current=Some((start,ch));
        while let Some((index,ch))=current {
            end=index+ch.len_utf8();
            if matches!(ch,'\''|'"'|'`') {
                for (index,next) in chars.by_ref() {end=index+next.len_utf8();if next==ch {break;}expanded.push(next);}
            } else {expanded.push(ch);}
            if chars.peek().is_some_and(|(_,next)|terminator(*next)) {break;}current=chars.next();
        }
        words.push((command[start..end].to_owned(),expanded));
    }words
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn quoted_suffix_keeps_escape_for_validation() {assert_eq!(read_command_words("node \"${PLUGIN_ROOT}\"/../escape.mjs"),vec![("node".to_owned(),"node".to_owned()),("\"${PLUGIN_ROOT}\"/../escape.mjs".to_owned(),"${PLUGIN_ROOT}/../escape.mjs".to_owned())]);}
    #[test] fn default_timeout_and_invalid_numbers() {assert!(!is_valid_hook_timeout_seconds(f64::NAN));assert!(!is_valid_hook_timeout_seconds(0.0));assert!(is_valid_hook_timeout_seconds(600.0));}
}
