use crate::websearch::{types::{ConfigLoadResult,ConfigLoadFailureReason,RoutingStrategy},search::provider_entry_label};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum StatusLevel { Info,Warning,Error }
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
