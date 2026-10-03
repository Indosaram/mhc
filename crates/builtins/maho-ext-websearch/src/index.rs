use crate::websearch::{types::{ConfigLoadResult,ConfigLoadFailureReason,RoutingStrategy},search::provider_entry_label};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum StatusLevel { Info,Warning,Error }
pub type ProviderNativeBypass=std::sync::Arc<dyn Fn(Option<&maho_ext_api::Model>)->bool+Send+Sync>;
pub struct WebsearchExtension { pub home:std::path::PathBuf,pub provider_native_bypass:ProviderNativeBypass }
impl maho_ext_api::Extension for WebsearchExtension {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
        let state=std::sync::Arc::new(std::sync::Mutex::new(ConfigLoadResult::Err{reason:ConfigLoadFailureReason::MissingConfig,message:"Missing websearch config. Create .pi/websearch.json or ~/.pi/websearch.json before starting pi.".into(),source:None}));
        let captured=state.clone(); let get_state:std::sync::Arc<dyn Fn()->ConfigLoadResult+Send+Sync>=std::sync::Arc::new(move ||captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
        if let Err(error)=api.register_tool_with_renderers(crate::websearch::tool::create_web_search_tool(get_state.clone()),crate::websearch::renderers::renderers()) {std::panic::panic_any(error);}
        register_websearch_command(api,get_state);
        for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::ModelSelect] {
            let state=state.clone();let home=self.home.clone();let bypass=self.provider_native_bypass.clone();
            api.on(event,std::sync::Arc::new(move |event,ctx| {
                let state=state.clone();let home=home.clone();let bypass=bypass.clone();
                Box::pin(async move {
                    let model=match event {maho_ext_api::ExtensionEvent::ModelSelect(event)=>Some(&event.model),_=>ctx.model.as_ref()};
                    let next=refresh_state(&ctx.cwd,&home,bypass(model)).await?;
                    *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=next.clone();
                    clear_ui(ctx);
                    if ctx.has_ui && let ConfigLoadResult::Err{reason,message,..}=&next && *reason!=ConfigLoadFailureReason::ProviderNativeBypass {ctx.ui.notify(message,maho_ext_api::NotificationType::Error);}
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
        api.on(maho_ext_api::EventKind::SessionShutdown,std::sync::Arc::new(|_,ctx|Box::pin(async move {clear_ui(ctx);Ok(maho_ext_api::EventResult::None)})));
    }
}
fn clear_ui(ctx:&maho_ext_api::ExtensionContext) {
    if ctx.has_ui {ctx.ui.set_status("pi-websearch",None);ctx.ui.set_widget("pi-websearch",None,Default::default());}
}
async fn refresh_state(cwd:&std::path::Path,home:&std::path::Path,bypass:bool)->Result<ConfigLoadResult,maho_ext_api::ExtensionFailure> {
    if bypass {Ok(ConfigLoadResult::Err{reason:ConfigLoadFailureReason::ProviderNativeBypass,message:"Native provider web search is handled by the built-in provider extension.".into(),source:None})}
    else {crate::websearch::config::load_websearch_config(cwd,home).await.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))}
}
pub fn register_websearch_command(api:&mut maho_ext_api::ExtensionApi,get_state:std::sync::Arc<dyn Fn()->ConfigLoadResult+Send+Sync>) {
    api.register_command("websearch",Some("Show web search provider status".into()),None,std::sync::Arc::new(move |raw,ctx| {
        let get_state=get_state.clone();
        Box::pin(async move {
            let (message,level)=status_message(raw,&get_state());
            ctx.ui.notify(&message,match level {StatusLevel::Info=>maho_ext_api::NotificationType::Info,StatusLevel::Warning=>maho_ext_api::NotificationType::Warning,StatusLevel::Error=>maho_ext_api::NotificationType::Error});
            Ok(())
        })
    }));
}
pub fn status_message(raw_args:&str,state:&ConfigLoadResult)->(String,StatusLevel) {
    let args=raw_args.trim_matches(|character:char|matches!(character,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'));
    if !args.is_empty() && args!="status" { return ("Usage: /websearch status".into(),StatusLevel::Warning); }
    match state {
        ConfigLoadResult::Ok{config,..}=>{ let strategy=match config.strategy { RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first" }; let providers=config.providers.iter().map(|entry|provider_entry_label(entry.config.provider.as_str(),entry.config.id.as_deref(),None)).collect::<Vec<_>>().join(", "); (format!("Web search active: strategy={strategy}, auto={}, providers={providers}",if config.auto { "enabled" } else { "disabled" }),StatusLevel::Info) },
        ConfigLoadResult::Err{reason:ConfigLoadFailureReason::ProviderNativeBypass,message,..}=>(format!("Web search deferred: {message}"),StatusLevel::Info),
        ConfigLoadResult::Err{message,..}=>(format!("Web search inactive: {message}"),StatusLevel::Error),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn lifecycle_refresh_bypass_precedes_config_and_registration_is_lazy() {
        let temp=tempfile::tempdir().unwrap();let cwd=temp.path().join("project");let home=temp.path().join("home");
        tokio::fs::create_dir_all(cwd.join(".senpi")).await.unwrap();tokio::fs::write(cwd.join(".senpi/websearch.json"),"invalid").await.unwrap();
        assert!(matches!(refresh_state(&cwd,&home,true).await.unwrap(),ConfigLoadResult::Err{reason:ConfigLoadFailureReason::ProviderNativeBypass,..}));
        assert!(matches!(refresh_state(&cwd,&home,false).await.unwrap(),ConfigLoadResult::Err{reason:ConfigLoadFailureReason::InvalidConfig,..}));
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("websearch",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        maho_ext_api::Extension::register(&WebsearchExtension{home,provider_native_bypass:std::sync::Arc::new(|_|panic!("registration must not probe native model"))},&mut api);
        assert_eq!(api.registered.tools[0].definition.name,"web_search");assert_eq!(api.registered.commands[0].name,"websearch");
        for event in [maho_ext_api::EventKind::SessionStart,maho_ext_api::EventKind::ModelSelect,maho_ext_api::EventKind::SessionShutdown] {assert_eq!(api.registered.handlers[&event].len(),1);}
    }
    #[test] fn status_argument_uses_ecmascript_whitespace() {
        let state=ConfigLoadResult::Err{reason:ConfigLoadFailureReason::ProviderNativeBypass,message:"native".into(),source:None};
        assert_eq!(status_message("\u{feff}status\u{feff}",&state).1,StatusLevel::Info);
        assert_eq!(status_message("\u{0085}status",&state).1,StatusLevel::Warning);
    }
    #[test] fn status_command_registers_native_handler() {
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("websearch",std::path::PathBuf::new(),Default::default()),Default::default(),Default::default(),Default::default());
        register_websearch_command(&mut api,std::sync::Arc::new(||ConfigLoadResult::Err{reason:ConfigLoadFailureReason::MissingConfig,message:"missing".into(),source:None}));
        assert!(api.registered.commands.iter().any(|command|command.name=="websearch"));
    }
    #[test] fn status_reason_controls_notification_level() { let state=ConfigLoadResult::Err{reason:ConfigLoadFailureReason::ProviderNativeBypass,message:"native".into(),source:None}; assert_eq!(status_message("status",&state).1,StatusLevel::Info); assert_eq!(status_message("reload",&state).1,StatusLevel::Warning); let state=ConfigLoadResult::Err{reason:ConfigLoadFailureReason::MissingConfig,message:"missing".into(),source:None}; assert_eq!(status_message("",&state).1,StatusLevel::Error); }
}
