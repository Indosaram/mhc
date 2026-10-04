use std::{path::PathBuf,sync::{Arc,Mutex}};
#[derive(Clone)]
pub struct MemoryStaticOptions{
    pub register_host:Option<Arc<dyn Fn(&mut maho_ext_api::ExtensionApi)+Send+Sync>>,
    pub skills_trackers:Option<crate::skills_usage_wiring::SkillsUsageTrackers>,
    pub prompt_handler:Option<Arc<crate::prompt::MemoryPromptHandler>>,
    pub prompt:crate::prompt::MemoryPromptInjectionOptions,
    pub nudge:Arc<Mutex<crate::nudge_wiring::MemoryNudgeWiring>>,
    pub resolve_context:crate::prompt::PromptContextResolver,
    pub resolve_tool_context:crate::tools::MemoryContextResolver,
    pub resolve_cwd:Arc<dyn Fn()->PathBuf+Send+Sync>,
    pub captured_tools:Arc<dyn Fn()->Vec<String>+Send+Sync>,
    pub warn:crate::guard::GuardWarning,
    pub edit_notice:Arc<dyn Fn(&str)->bool+Send+Sync>,
    pub theme:crate::worker::completion_renderers::ResolveEntryTheme,
    pub memory_write:crate::wiring_memory_write::MemoryWriteOptions,
    pub skills_usage:crate::skills_usage_wiring::SkillsUsageOptions,
    pub triggers:crate::trigger_wiring::ReflectionTriggerWiringOptions,
}
pub struct MemoryStaticRegistration{
    pub skills_usage:crate::skills_usage_wiring::SkillsUsageTrackers,
    pub triggers:Arc<crate::trigger_wiring::ReflectionTriggerWiring>,
}
pub fn register_memory_static(api:&mut maho_ext_api::ExtensionApi,options:MemoryStaticOptions)->MemoryStaticRegistration{
    if let Some(register)=options.register_host{register(api);}
    crate::worker::completion_renderers::register_reflection_completion_renderer(api,options.theme.clone());
    crate::worker::health_alert::register_reflection_health_renderer(api,options.theme);
    crate::nudge_wiring::MemoryNudgeWiring::register(options.nudge,api,options.resolve_context.clone());
    crate::soul_notice::register_soul_notice(api,options.resolve_context.clone(),options.edit_notice);
    match options.prompt_handler{Some(handler)=>crate::prompt::register_memory_prompt_handler_with_cache(api,options.prompt,handler),None=>crate::prompt::register_memory_prompt_handler(api,options.prompt)}
    crate::tools::register_memory_tools(api,options.resolve_tool_context);
    crate::guard::register_memory_guard(api,options.resolve_context.clone(),options.resolve_cwd,options.captured_tools,options.warn);
    crate::skills_scope::register_memory_skills_scope(api,options.resolve_context);
    let skills_usage=match options.skills_trackers{Some(trackers)=>{crate::skills_usage_wiring::register_skills_usage_with_trackers(api,options.skills_usage,trackers.clone());trackers},None=>crate::skills_usage_wiring::register_skills_usage(api,options.skills_usage)};
    crate::wiring_memory_write::register_memory_write_listener(api,options.memory_write);
    let triggers=crate::trigger_wiring::create_reflection_trigger_wiring(options.triggers);triggers.register(api);
    MemoryStaticRegistration{skills_usage,triggers}
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn native_static_registration_installs_tools_and_lifecycle_handlers(){
        struct Theme;impl crate::worker::entry_renderers::EntryRenderTheme for Theme{fn fg(&self,_:&str,text:&str)->String{text.into()}fn italic(&self,text:&str)->String{text.into()}}
        let resolve:crate::prompt::PromptContextResolver=Arc::new(|_|None);let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        let registration=register_memory_static(&mut api,MemoryStaticOptions{
            register_host:None,
            skills_trackers:None,
            prompt_handler:None,
            prompt:crate::prompt::MemoryPromptInjectionOptions{resolve_context:resolve.clone(),create_repo:None,search_exposure:None,resolve_nudge_turns:None,resolve_soul_notice:None},nudge:Arc::new(Mutex::new(Default::default())),resolve_context:resolve,resolve_tool_context:Arc::new(||None),resolve_cwd:Arc::new(PathBuf::new),captured_tools:Arc::new(Vec::new),warn:Arc::new(|_|{}),edit_notice:Arc::new(|_|true),theme:Arc::new(|_|Arc::new(Theme)),
            memory_write:crate::wiring_memory_write::MemoryWriteOptions{resolve_session:Arc::new(|_|None),on_memory_write:Arc::new(|_|Ok(())),refresh_status:Arc::new(|_,_|panic!("unbound"))},
            skills_usage:crate::skills_usage_wiring::SkillsUsageOptions{resolve_context:Arc::new(|_|None),resolve_cwd:Arc::new(PathBuf::new),now_ms:Arc::new(||0)},
            triggers:crate::trigger_wiring::ReflectionTriggerWiringOptions{resolve_session:Arc::new(|_|None),on_launch:Arc::new(|_|Ok(())),warn:Arc::new(|_|{})},
        });
        for kind in [maho_ext_api::EventKind::BeforeAgentStart,maho_ext_api::EventKind::InputDisposition,maho_ext_api::EventKind::ToolCall,maho_ext_api::EventKind::ToolResult,maho_ext_api::EventKind::ResourcesDiscover,maho_ext_api::EventKind::AgentSettled]{assert!(api.registered.handlers.contains_key(&kind));}
        assert!(api.registered.entry_renderers.contains_key(crate::worker::health_alert::REFLECTION_HEALTH_ENTRY_TYPE));assert!(registration.skills_usage.lock().unwrap().is_empty());
    }
}
