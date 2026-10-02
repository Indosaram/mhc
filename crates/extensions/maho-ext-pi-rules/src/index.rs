use std::{collections::BTreeSet,path::Path,sync::{Arc,Mutex}};
use maho_ext_api::{BeforeAgentStartEventResult,EventKind,EventResult,ExtensionApi,ExtensionEvent,ExtensionFailure,ExtensionRuntime,FlagType,FlagValue,ToolContent,ToolResultEventResult};
use crate::{config::config_from_environment,rules::{engine::{Engine,EngineDeps},finder::{FinderOptions,find_rule_candidates},project_root::find_project_root,tool_paths::extract_tool_paths,types::{Mode,RuleCandidate}}};
pub struct FilesystemDeps;
impl EngineDeps for FilesystemDeps{
    fn find_candidates(&mut self,options:FinderOptions<'_>)->Vec<RuleCandidate>{find_rule_candidates(options)}
    fn read_file(&mut self,path:&str)->Option<String>{std::fs::read_to_string(path).ok()}
    fn find_project_root(&mut self,path:&str)->Option<String>{find_project_root(Path::new(path),None).map(|path|path.to_string_lossy().into_owned())}
}
fn sync_flags(engine:&mut Engine<FilesystemDeps>,runtime:&ExtensionRuntime,env_disabled:bool){
    if let Some(FlagValue::Boolean(disabled))=runtime.get_flag("pi-rules-disabled"){engine.config.disabled=disabled||env_disabled;}
    if let Some(FlagValue::String(mode))=runtime.get_flag("pi-rules-mode")
        && let Some(mode)=match mode.as_str(){"static"=>Some(Mode::Static),"dynamic"=>Some(Mode::Dynamic),"both"=>Some(Mode::Both),"off"=>Some(Mode::Off),_=>None}{engine.config.mode=mode;}
}
pub fn register_rule_injection_hooks(api:&mut ExtensionApi){
    api.register_flag("pi-rules-disabled",FlagType::Boolean{default:Some(false)},Some("Disable pi-rules hooks.".into()));
    api.register_flag("pi-rules-mode",FlagType::String{default:Some("both".into())},Some("Rule injection mode: static, dynamic, both, or off.".into()));
    let config=config_from_environment();let env_disabled=config.disabled;let engine=Arc::new(Mutex::new(Engine::new(config,FilesystemDeps)));
    for kind in [EventKind::SessionStart,EventKind::SessionCompact]{
        let engine=Arc::clone(&engine);let runtime=api.runtime.clone();
        api.on(kind,Arc::new(move|_,ctx|{let mut engine=engine.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if kind==EventKind::SessionStart{sync_flags(&mut engine,&runtime,env_disabled);}engine.reset_session(Some(&ctx.cwd.to_string_lossy()));Box::pin(async{Ok(EventResult::None)})}));
    }
    let static_engine=Arc::clone(&engine);let runtime=api.runtime.clone();
    api.on(EventKind::BeforeAgentStart,Arc::new(move|event,ctx|{
        let engine=Arc::clone(&static_engine);let runtime=runtime.clone();
        Box::pin(async move{
            let ExtensionEvent::BeforeAgentStart(event)=event else{return Ok(EventResult::None);};let mut engine=engine.lock().unwrap_or_else(std::sync::PoisonError::into_inner);sync_flags(&mut engine,&runtime,env_disabled);
            if engine.config.disabled||matches!(engine.config.mode,Mode::Off|Mode::Dynamic){return Ok(EventResult::None);}
            let loaded=engine.load_static_rules(&ctx.cwd.to_string_lossy());let mut paths=BTreeSet::new();
            for file in &event.system_prompt_options.context_files{paths.insert(file.path.clone());if let Ok(path)=std::fs::canonicalize(&file.path){paths.insert(path.to_string_lossy().into_owned());}}
            for rule in &loaded.rules{if paths.contains(&rule.candidate.path)||paths.contains(&rule.candidate.real_path){engine.mark_static_injected(rule);}}
            let rules=loaded.rules.into_iter().filter(|rule|!paths.contains(&rule.candidate.path)&&!paths.contains(&rule.candidate.real_path)&&!engine.is_static_injected(rule)).collect::<Vec<_>>();
            if rules.is_empty(){return Ok(EventResult::None);}let block=engine.format_static(&rules);for rule in &rules{engine.mark_static_injected(rule);}
            Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult{system_prompt:Some(format!("{}{block}",event.system_prompt)),message:None}))
        })
    }));
    let runtime=api.runtime.clone();
    api.on(EventKind::ToolResult,Arc::new(move|event,ctx|{
        let engine=Arc::clone(&engine);let runtime=runtime.clone();
        Box::pin(async move{
            let ExtensionEvent::ToolResult(event)=event else{return Ok(EventResult::None);};let mut engine=engine.lock().unwrap_or_else(std::sync::PoisonError::into_inner);sync_flags(&mut engine,&runtime,env_disabled);
            if engine.config.disabled||matches!(engine.config.mode,Mode::Off|Mode::Static)||event.is_error{return Ok(EventResult::None);}
            let targets=extract_tool_paths(&event.tool_name,event.is_error,&event.input,event.details.as_ref(),&ctx.cwd).into_iter().map(|path|path.to_string_lossy().into_owned()).collect::<Vec<_>>();let Some(first)=targets.first()else{return Ok(EventResult::None);};
            let cwd=ctx.cwd.to_string_lossy();let fingerprints=engine.fingerprint_dynamic_targets(&cwd,&targets);let pending=fingerprints.iter().filter(|target|!engine.is_dynamic_target_fingerprint_current(target)).map(|target|target.target_path.clone()).collect::<Vec<_>>();
            if pending.is_empty(){engine.commit_dynamic_target_fingerprints(&fingerprints);return Ok(EventResult::None);}
            let loaded=engine.load_dynamic_rules(&cwd,&pending).map_err(|error|ExtensionFailure::new(error.to_string()))?;engine.commit_dynamic_target_fingerprints(&fingerprints);
            let rules=loaded.rules.into_iter().filter(|rule|!engine.is_static_injected(rule)&&!engine.is_dynamic_injected(first,rule)).collect::<Vec<_>>();if rules.is_empty(){return Ok(EventResult::None);}
            let target=&pending[0];let display=display_path(&ctx.cwd,Path::new(target));let block=engine.format_dynamic(&rules,&display);for rule in &rules{engine.mark_dynamic_injected(first,rule);}
            let mut content=event.content.clone();content.push(ToolContent::text(block));Ok(EventResult::ToolResult(ToolResultEventResult{content:Some(content),details:None,is_error:None,usage:None}))
        })
    }));
}
fn display_path(cwd:&Path,target:&Path)->String{
    if !target.is_absolute(){return target.to_string_lossy().into_owned();}
    let left=cwd.components().collect::<Vec<_>>();let right=target.components().collect::<Vec<_>>();let common=left.iter().zip(&right).take_while(|(left,right)|left==right).count();let mut path=std::path::PathBuf::new();
    for _ in common..left.len(){path.push("..");}for component in &right[common..]{path.push(component.as_os_str());}path.to_string_lossy().into_owned()
}
