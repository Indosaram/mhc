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
    fn plugin_handler(root:&std::path::Path,command:&str,windows:Option<&str>)->ExecutableHookHandler {
        let source=crate::types::HookSourceMetadata {scope:HookSourceScope::Plugin,source_path:root.join("hooks/hooks.json").to_string_lossy().into_owned(),display_order:0,discovered_at:crate::types::HookDiscoveryTiming::PreSession,plugin_root:Some(root.to_string_lossy().into_owned()),manifest_path:None,plugin_env:Some(crate::plugin_manifest::build_plugin_env(root,None).unwrap())};
        crate::schema::parse_hook_config(&serde_json::json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":command,"commandWindows":windows}]}]}}),&source).executable_handlers.remove(0)
    }
    #[test]
    fn plugin_targets_reject_escaped_and_quoted_suffixes_in_both_fields()->std::io::Result<()> {
        let root=tempfile::tempdir()?;
        for (command,windows) in [("node ${PLUGIN_ROOT}/../escape.mjs","node ${PLUGIN_ROOT}\\..\\escape.ps1"),("node \"${PLUGIN_ROOT}\"/../escape.mjs","node \"%PLUGIN_ROOT%\"/../escape.cmd")] {
            let diagnostics=validate_hook_handler_safety(&plugin_handler(root.path(),command,Some(windows)))?;assert_eq!(diagnostics.len(),2);assert!(diagnostics.iter().all(|diagnostic|diagnostic.code=="invalid_command_target"));assert!(diagnostics[0].path.ends_with(".command"));assert!(diagnostics[1].path.ends_with(".commandWindows"));
        }Ok(())
    }
    #[test]
    fn missing_plugin_target_returns_diagnostic()->std::io::Result<()> {
        let root=tempfile::tempdir()?;let diagnostics=validate_hook_handler_safety(&plugin_handler(root.path(),"node ${PLUGIN_ROOT}/hooks/missing.mjs",Some("exit 0")))?;assert_eq!(diagnostics.len(),1);assert_eq!(diagnostics[0].code,"missing_command_target");Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn command_target_uses_filesystem_truth_through_symlink_dotdot()->std::io::Result<()> {
        for decoy in [true,false] {
            let root=tempfile::tempdir()?;let outside=tempfile::tempdir()?;std::fs::create_dir(outside.path().join("subdir"))?;std::fs::write(outside.path().join("escape.mjs"),b"exit 0")?;
            if decoy {std::fs::write(root.path().join("escape.mjs"),b"exit 1")?;}
            std::os::unix::fs::symlink(outside.path().join("subdir"),root.path().join("jump"))?;std::os::unix::fs::symlink("jump/..",root.path().join("entry"))?;
            let diagnostics=validate_hook_handler_safety(&plugin_handler(root.path(),"node ${PLUGIN_ROOT}/entry/escape.mjs",Some("exit 0")))?;assert_eq!(diagnostics.len(),1);assert_eq!(diagnostics[0].code,"invalid_command_target");
        }Ok(())
    }
    #[test]
    fn environment_inherits_only_minimal_explicit_and_plugin_fields()->std::io::Result<()> {
        let root=tempfile::tempdir()?;let handler=plugin_handler(root.path(),"exit 0",Some("exit 0"));let source=BTreeMap::from([("PATH".to_owned(),"/bin".to_owned()),("HOME".to_owned(),"home".to_owned()),("ALLOWED".to_owned(),"allow".to_owned()),("DENIED".to_owned(),"deny".to_owned()),("PLUGIN_ROOT".to_owned(),"wrong".to_owned())]);
        let env=build_hook_environment(&handler,SupportedHookEvent::PreToolUse,&source,&["ALLOWED".to_owned()]);assert_eq!(env["PATH"],"/bin");assert_eq!(env["HOME"],"home");assert_eq!(env["ALLOWED"],"allow");assert!(!env.contains_key("DENIED"));assert_eq!(env["PLUGIN_ROOT"],root.path().to_string_lossy());assert_eq!(env["CLAUDE_PLUGIN_ROOT"],env["PLUGIN_ROOT"]);assert_eq!(env["SENPI_HOOK_EVENT"],"PreToolUse");assert_eq!(env["SENPI_HOOK_SOURCE"],handler.source.source_path);Ok(())
    }
    #[test] fn quoted_suffix_keeps_escape_for_validation() {assert_eq!(read_command_words("node \"${PLUGIN_ROOT}\"/../escape.mjs"),vec![("node".to_owned(),"node".to_owned()),("\"${PLUGIN_ROOT}\"/../escape.mjs".to_owned(),"${PLUGIN_ROOT}/../escape.mjs".to_owned())]);}
    #[test] fn default_timeout_and_invalid_numbers() {assert!(!is_valid_hook_timeout_seconds(f64::NAN));assert!(!is_valid_hook_timeout_seconds(0.0));assert!(is_valid_hook_timeout_seconds(600.0));}
}
