use serde_json::{Value,json};
use crate::diagnostics::{DiagnosticDraft,diagnostic};
use crate::schema::parse_hook_config;
use crate::types::{HookDiscoveryTiming,HookSourceMetadata,HookSourceScope,ParsedHookConfig,Severity,SupportedHookEvent};

#[derive(Default)]
pub struct HookConfigLoaderOptions {
    pub cwd:String,
    pub agent_dir:String,
    pub global_settings_hooks:Option<Value>,
    pub project_settings_hooks:Option<Value>,
    pub global_hooks_path:Option<String>,
    pub project_hooks_path:Option<String>,
    pub global_hook_source_paths:Vec<String>,
    pub project_hook_source_paths:Vec<String>,
    pub pre_session_hook_source_paths:Vec<String>,
    pub runtime_hook_source_paths:Vec<String>,
}

enum SourceContent { Inline(Value), File }
struct SourceCandidate { content:SourceContent,source:HookSourceMetadata }

fn source(scope:HookSourceScope,path:String,timing:HookDiscoveryTiming)->HookSourceMetadata {
    HookSourceMetadata {scope,source_path:path,discovered_at:timing,display_order:0,plugin_root:None,manifest_path:None,plugin_env:None}
}

fn create_source_candidates(options:&HookConfigLoaderOptions)->Vec<SourceCandidate> {
    use HookDiscoveryTiming::{PreSession,Runtime};use HookSourceScope as S;
    let mut candidates=Vec::new();
    if let Some(hooks)=&options.global_settings_hooks {candidates.push(SourceCandidate {content:SourceContent::Inline(hooks.clone()),source:source(S::Global,"<global-settings-hooks>".to_owned(),PreSession)});}
    candidates.push(SourceCandidate {content:SourceContent::File,source:source(S::Global,options.global_hooks_path.clone().unwrap_or_else(||format!("{}/hooks.json",options.agent_dir)),PreSession)});
    for path in &options.global_hook_source_paths {candidates.push(SourceCandidate {content:SourceContent::File,source:source(S::Global,path.clone(),PreSession)});}
    if let Some(hooks)=&options.project_settings_hooks {candidates.push(SourceCandidate {content:SourceContent::Inline(hooks.clone()),source:source(S::Project,"<project-settings-hooks>".to_owned(),PreSession)});}
    candidates.push(SourceCandidate {content:SourceContent::File,source:source(S::Project,options.project_hooks_path.clone().unwrap_or_else(||format!("{}/.senpi/hooks.json",options.cwd)),PreSession)});
    for (scope,paths,timing) in [(S::Project,&options.project_hook_source_paths,PreSession),(S::Plugin,&options.pre_session_hook_source_paths,PreSession),(S::Runtime,&options.runtime_hook_source_paths,Runtime)] {
        for path in paths {candidates.push(SourceCandidate {content:SourceContent::File,source:source(scope.clone(),path.clone(),timing.clone())});}
    }
    candidates
}

pub fn load_hook_config_sources(options:&HookConfigLoaderOptions,mut read_text_file:impl FnMut(&str)->std::io::Result<Option<String>>)->ParsedHookConfig {
    let mut parsed=ParsedHookConfig::default();let mut display_order=0;
    for mut candidate in create_source_candidates(options) {
        candidate.source.display_order=display_order;
        let loaded=match candidate.content {
            SourceContent::Inline(hooks)=>parse_hook_config(&json!({"hooks":hooks}),&candidate.source),
            SourceContent::File=>match read_text_file(&candidate.source.source_path) {
                Ok(None)=>continue,
                Ok(Some(text))=>match serde_json::from_str::<Value>(&text) {
                    Ok(value)=>parse_hook_config(&value,&candidate.source),
                    Err(error)=>source_error(&candidate.source,format!("Hook source JSON is malformed: {error}")),
                },
                Err(error)=>source_error(&candidate.source,format!("Hook source could not be read: {error}")),
            },
        };
        display_order+=1;
        if candidate.source.discovered_at==HookDiscoveryTiming::Runtime && loaded.executable_handlers.iter().any(|h|h.event==SupportedHookEvent::SessionStart) {
            let mut diagnostics=loaded.diagnostics;
            diagnostics.push(diagnostic(DiagnosticDraft {code:"unsupported_event",event:Some("SessionStart"),message:"Runtime SessionStart hooks are loaded for reload or the next session only.".to_owned(),path:"hooks.SessionStart".to_owned(),severity:Some(Severity::Warning)},&candidate.source));
            parsed.executable_handlers.extend(loaded.executable_handlers);parsed.diagnostics.extend(diagnostics);
        } else {parsed.executable_handlers.extend(loaded.executable_handlers);parsed.diagnostics.extend(loaded.diagnostics);}
    }
    parsed
}

pub fn load_hook_config_files(options:&HookConfigLoaderOptions)->ParsedHookConfig {
    load_hook_config_sources(options,|path|match std::fs::read(path) {
        Ok(bytes)=>Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(None),Err(error)=>Err(error),
    })
}

pub async fn load_hook_config_sources_async(options:&HookConfigLoaderOptions)->ParsedHookConfig {
    let reads=create_source_candidates(options).into_iter().filter_map(|candidate|match candidate.content {
        SourceContent::Inline(_)=>None,
        SourceContent::File=>Some(async move {
            let path=candidate.source.source_path;
            let result=match tokio::fs::read(&path).await {
                Ok(bytes)=>Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>Ok(None),Err(error)=>Err(error),
            };(path,result)
        }),
    });
    let files=futures::future::join_all(reads).await.into_iter().collect::<std::collections::BTreeMap<_,_>>();
    load_hook_config_sources(options,|path|match files.get(path) {
        Some(Ok(text))=>Ok(text.clone()),Some(Err(error))=>Err(std::io::Error::new(error.kind(),error.to_string())),None=>Ok(None),
    })
}

fn source_error(source:&HookSourceMetadata,message:String)->ParsedHookConfig {
    ParsedHookConfig {executable_handlers:Vec::new(),diagnostics:vec![diagnostic(DiagnosticDraft {code:"invalid_root",message,path:"$".to_owned(),event:None,severity:None},source)]}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn sync_and_async_files_decode_invalid_utf8_with_replacement()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("hooks.json");
        let mut bytes=b"{\"hooks\":{\"Stop\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"echo ".to_vec();bytes.push(0xff);bytes.extend_from_slice(b"\"}]}]}}");std::fs::write(&path,bytes)?;
        let options=HookConfigLoaderOptions {global_hooks_path:Some(path.to_string_lossy().into_owned()),project_hooks_path:Some(dir.path().join("absent").to_string_lossy().into_owned()),..Default::default()};
        let sync=load_hook_config_files(&options);let asynchronous=load_hook_config_sources_async(&options).await;
        assert!(sync.diagnostics.is_empty());assert!(asynchronous.diagnostics.is_empty());assert_eq!(sync.executable_handlers.len(),1);assert_eq!(sync.executable_handlers[0].config.command,"echo \u{fffd}");assert_eq!(asynchronous.executable_handlers,sync.executable_handlers);Ok(())
    }
    use std::collections::BTreeMap;
    fn hooks(command:&str,event:&str)->Value {json!({event:[{"matcher":"*","hooks":[{"type":"command","command":command}]}]})}

    #[tokio::test]
    async fn asynchronous_files_preserve_order_and_neighbor_diagnostics()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let global=dir.path().join("global.json");let project=dir.path().join("project.json");let runtime=dir.path().join("runtime.json");
        std::fs::write(&global,json!({"hooks":hooks("global","PreToolUse")}).to_string())?;
        std::fs::write(&project,"{")?;
        std::fs::write(&runtime,json!({"hooks":hooks("runtime","SessionStart")}).to_string())?;
        let options=HookConfigLoaderOptions {global_hooks_path:Some(global.to_string_lossy().into_owned()),project_hooks_path:Some(project.to_string_lossy().into_owned()),runtime_hook_source_paths:vec![runtime.to_string_lossy().into_owned()],..Default::default()};
        let parsed=load_hook_config_sources_async(&options).await;
        assert_eq!(parsed.executable_handlers.iter().map(|handler|handler.config.command.as_str()).collect::<Vec<_>>(),["global","runtime"]);
        assert_eq!(parsed.diagnostics.iter().map(|diagnostic|diagnostic.code.as_str()).collect::<Vec<_>>(),["invalid_root","unsupported_event"]);Ok(())
    }

    #[test]
    fn canonical_source_order_is_stable() {
        let files:BTreeMap<_,_>=[("/home/agent/hooks.json","global-file"),("/repo/.senpi/hooks.json","project-file"),("/repo/plugin-a/hooks.json","pre-session-a"),("/repo/plugin-b/hooks.json","pre-session-b"),("/repo/runtime/hooks.json","runtime")].into_iter().map(|(path,command)|(path,json!({"hooks":hooks(command,"PreToolUse")}).to_string())).collect();
        let options=HookConfigLoaderOptions {agent_dir:"/home/agent".to_owned(),cwd:"/repo".to_owned(),global_settings_hooks:Some(hooks("global-inline","PreToolUse")),project_settings_hooks:Some(hooks("project-inline","PreToolUse")),pre_session_hook_source_paths:vec!["/repo/plugin-a/hooks.json".to_owned(),"/repo/plugin-b/hooks.json".to_owned()],runtime_hook_source_paths:vec!["/repo/runtime/hooks.json".to_owned()],..Default::default()};
        let parsed=load_hook_config_sources(&options,|path|Ok(files.get(path).cloned()));assert!(parsed.diagnostics.is_empty());
        assert_eq!(parsed.executable_handlers.iter().map(|h|h.config.command.as_str()).collect::<Vec<_>>(),["global-inline","global-file","project-inline","project-file","pre-session-a","pre-session-b","runtime"]);
        assert_eq!(parsed.executable_handlers.iter().map(|h|h.source.display_order).collect::<Vec<_>>(),(0..7).collect::<Vec<_>>());
        assert_eq!(parsed.executable_handlers[6].source.discovered_at,HookDiscoveryTiming::Runtime);
        assert_eq!(parsed.executable_handlers[4].source.scope,HookSourceScope::Plugin);
    }

    #[test]
    fn invalid_sources_keep_valid_neighbors_and_runtime_diagnostic() {
        let files:BTreeMap<_,_>=[("/home/agent/hooks.json","{".to_owned()),("/repo/.senpi/hooks.json",json!({"hooks":"bad"}).to_string()),("/repo/plugin/hooks.json",json!({"hooks":hooks("plugin-valid","PreToolUse")}).to_string()),("/repo/runtime/hooks.json",json!({"hooks":hooks("runtime-session-start","SessionStart")}).to_string())].into_iter().collect();
        let options=HookConfigLoaderOptions {agent_dir:"/home/agent".to_owned(),cwd:"/repo".to_owned(),global_settings_hooks:Some(hooks("global-valid","PreToolUse")),pre_session_hook_source_paths:vec!["/repo/plugin/hooks.json".to_owned()],runtime_hook_source_paths:vec!["/repo/runtime/hooks.json".to_owned()],..Default::default()};
        let parsed=load_hook_config_sources(&options,|path|Ok(files.get(path).cloned()));
        assert_eq!(parsed.executable_handlers.iter().map(|h|h.config.command.as_str()).collect::<Vec<_>>(),["global-valid","plugin-valid","runtime-session-start"]);
        assert_eq!(parsed.diagnostics.iter().map(|d|d.code.as_str()).collect::<Vec<_>>(),["invalid_root","invalid_hooks","unsupported_event"]);
        assert_eq!(parsed.diagnostics[2].source.discovered_at,HookDiscoveryTiming::Runtime);assert_eq!(parsed.diagnostics[2].path,"hooks.SessionStart");
    }
}
