use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path,PathBuf};
use crate::diagnostics::{DiagnosticDraft,diagnostic};
use crate::plugin_manifest::{LoadPluginHookManifestOptions,build_plugin_env,source_for_path,resolve_contained_path,MANIFEST_PATH,DEFAULT_HOOK_PATH};
use crate::schema::parse_hook_config;
use crate::safety::validate_hook_handler_safety;
use crate::types::{ExecutableHookHandler,HookDiagnostic,HookSourceMetadata,ParsedHookConfig};

pub struct PluginHookManifestLoadResult {
    pub sources:Vec<HookSourceMetadata>,
    pub parsed:ParsedHookConfig,
    pub diagnostics:Vec<HookDiagnostic>,
}

struct HookSourceDraft {key:PathBuf,config:Value}

pub fn load_plugin_hook_manifest(options:&LoadPluginHookManifestOptions)->std::io::Result<PluginHookManifestLoadResult> {
    let root=std::path::absolute(&options.plugin_root)?;let manifest=root.join(MANIFEST_PATH);let env=build_plugin_env(&root,options.data_root.as_deref())?;
    let mut diagnostics=Vec::new();let mut drafts=Vec::new();
    if !root.is_dir() {
        add(&mut diagnostics,options,&env,&manifest,"$.pluginRoot",format!("Plugin root does not exist: {}",root.display()))?;
        return Ok(PluginHookManifestLoadResult {sources:Vec::new(),parsed:ParsedHookConfig {executable_handlers:Vec::new(),diagnostics:diagnostics.clone()},diagnostics});
    }
    if manifest.is_file() && let Some(value)=read_json(&manifest,options,&env,&mut diagnostics)? {
        if !value.is_object() {add(&mut diagnostics,options,&env,&manifest,"$","Plugin manifest must be an object.".to_owned())?;}
        else if let Some(hooks)=value.get("hooks") {normalize_hook_drafts(hooks,"$.hooks",options,&env,&manifest,&mut diagnostics,&mut drafts)?;}
    }
    if options.include_default_hooks && let Ok(path)=resolve_contained_path(&root,DEFAULT_HOOK_PATH) && path.is_file()
        && let Some(value)=read_json(&path,options,&env,&mut diagnostics)? {drafts.push(HookSourceDraft {key:path,config:value});}
    let mut sources=Vec::new();let mut parsed=ParsedHookConfig::default();
    for (index,draft) in drafts.into_iter().enumerate() {
        let mut source=source_for_path(options,&env,&draft.key)?;source.display_order+=index;sources.push(source.clone());
        let loaded=parse_hook_config(&draft.config,&source);
        for handler in loaded.executable_handlers {
            let safety=validate_hook_handler_safety(&handler)?;
            if safety.is_empty() {parsed.executable_handlers.push(handler);} else {parsed.diagnostics.extend(safety);}
        }
        parsed.diagnostics.extend(loaded.diagnostics);
    }
    diagnostics.extend(parsed.diagnostics.clone());Ok(PluginHookManifestLoadResult {sources,parsed,diagnostics})
}

pub fn select_hook_command_for_platform<'a>(handler:&'a ExecutableHookHandler,platform:&str)->&'a str {
    if platform=="win32" {handler.config.command_windows.as_deref().unwrap_or(&handler.config.command)} else {&handler.config.command}
}

fn add(diagnostics:&mut Vec<HookDiagnostic>,options:&LoadPluginHookManifestOptions,env:&BTreeMap<String,String>,source:&Path,path:&str,message:String)->std::io::Result<()> {
    diagnostics.push(diagnostic(DiagnosticDraft {code:"invalid_root",message,path:path.to_owned(),event:None,severity:None},&source_for_path(options,env,source)?));Ok(())
}

fn read_json(path:&Path,options:&LoadPluginHookManifestOptions,env:&BTreeMap<String,String>,diagnostics:&mut Vec<HookDiagnostic>)->std::io::Result<Option<Value>> {
    let value=std::fs::read_to_string(path).and_then(|text|serde_json::from_str(&text).map_err(std::io::Error::other));
    match value {Ok(value)=>Ok(Some(value)),Err(error)=>{add(diagnostics,options,env,path,"$",format!("Could not read plugin hook JSON: {error}"))?;Ok(None)}}
}

fn normalize_hook_drafts(value:&Value,path:&str,options:&LoadPluginHookManifestOptions,env:&BTreeMap<String,String>,manifest:&Path,diagnostics:&mut Vec<HookDiagnostic>,drafts:&mut Vec<HookSourceDraft>)->std::io::Result<()> {
    match value {
        Value::String(input)=>match resolve_contained_path(&options.plugin_root,input) {
            Ok(resolved) if resolved.is_file()=>{if let Some(value)=read_json(&resolved,options,env,diagnostics)? {drafts.push(HookSourceDraft {key:resolved,config:value});}}
            Ok(resolved)=>add(diagnostics,options,env,&resolved,path,format!("Plugin hook file does not exist: {}",resolved.display()))?,
            Err(error)=>add(diagnostics,options,env,manifest,path,error.to_string())?,
        },
        Value::Array(values)=>for (index,value) in values.iter().enumerate() {normalize_hook_drafts(value,&format!("{path}[{index}]"),options,env,manifest,diagnostics,drafts)?;},
        Value::Object(_)=>drafts.push(HookSourceDraft {key:PathBuf::from(format!("{}#{}",manifest.display(),path.trim_start_matches('$').trim_start_matches('.'))),config:value.clone()}),
        Value::Null|Value::Bool(_)|Value::Number(_)=>add(diagnostics,options,env,manifest,path,"Plugin manifest hooks must be a path or hook object.".to_owned())?,
    }Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn hook(command:&str)->Value {json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":command}]}]}})}
    fn write_json(path:&Path,value:&Value)->std::io::Result<()> {std::fs::create_dir_all(path.parent().expect("parent"))?;std::fs::write(path,value.to_string())}
    fn options(root:&Path)->LoadPluginHookManifestOptions {LoadPluginHookManifestOptions {plugin_root:root.to_owned(),display_order:3,discovered_at:None,data_root:None,include_default_hooks:true}}
    #[test]
    fn paths_inline_defaults_and_root_metadata()->std::io::Result<()> {
        let root=tempfile::tempdir()?;let path=root.path();let command="node ${PLUGIN_ROOT}/hooks/one.mjs";
        write_json(&path.join("hooks/one.json"),&hook(command))?;std::fs::write(path.join("hooks/one.mjs"),"")?;
        write_json(&path.join("hooks/two.json"),&hook("two"))?;write_json(&path.join("hooks/hooks.json"),&hook("default"))?;
        write_json(&path.join(MANIFEST_PATH),&json!({"hooks":["./hooks/one.json",["hooks/two.json"],hook("inline-one"),[hook("inline-two")]]}))?;
        let loaded=load_plugin_hook_manifest(&options(path))?;assert!(loaded.diagnostics.is_empty());
        assert_eq!(loaded.parsed.executable_handlers.iter().map(|h|h.config.command.as_str()).collect::<Vec<_>>(),[command,"two","inline-one","inline-two","default"]);
        assert_eq!(loaded.sources[2].source_path,format!("{}#hooks[2]",path.join(MANIFEST_PATH).display()));
        for source in loaded.sources {assert_eq!(source.plugin_root.as_deref(),path.to_str());assert!(source.plugin_env.is_some());}Ok(())
    }
    #[test]
    fn malformed_missing_and_escaping_paths_rejected()->std::io::Result<()> {
        for (hooks,path) in [(json!("./hooks/missing.json"),"$.hooks"),(json!("../escape.json"),"$.hooks"),(json!([123]),"$.hooks[0]")] {
            let root=tempfile::tempdir()?;write_json(&root.path().join(MANIFEST_PATH),&json!({"hooks":hooks}))?;
            let loaded=load_plugin_hook_manifest(&options(root.path()))?;assert!(loaded.parsed.executable_handlers.is_empty());assert_eq!(loaded.diagnostics[0].path,path);
        }Ok(())
    }
    #[test]
    fn windows_paths_and_command_selection()->std::io::Result<()> {
        let root=tempfile::tempdir()?;write_json(&root.path().join(MANIFEST_PATH),&json!({"hooks":".\\hooks\\windows.json"}))?;
        write_json(&root.path().join("hooks/windows.json"),&json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"posix","commandWindows":"windows"}]}]}}))?;
        let loaded=load_plugin_hook_manifest(&options(root.path()))?;assert!(loaded.diagnostics.is_empty());
        assert_eq!(select_hook_command_for_platform(&loaded.parsed.executable_handlers[0],"win32"),"windows");assert_eq!(select_hook_command_for_platform(&loaded.parsed.executable_handlers[0],"darwin"),"posix");Ok(())
    }
}
