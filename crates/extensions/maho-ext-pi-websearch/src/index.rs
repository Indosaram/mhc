use std::sync::{Arc,Mutex};
use maho_ext_api::{EventKind,EventResult,ExtensionApi,ExtensionContext,ExtensionFailure,NotificationType};
use crate::websearch::{config::load_websearch_config,search::provider_entry_label,types::*};
const STATUS_KEY:&str="pi-websearch";
const NATIVE_BYPASS_MESSAGE:&str="Native provider web search is handled by the built-in provider extension.";
pub fn is_provider_native_bypass(provider:Option<&str>)->bool{matches!(provider,Some("openai"|"anthropic"))}
fn clear_ui(ctx:&ExtensionContext){if ctx.has_ui{ctx.ui.set_status(STATUS_KEY,None);ctx.ui.set_widget(STATUS_KEY,None,Default::default());}}
pub fn register_search_lifecycle(api:&mut ExtensionApi)->Arc<Mutex<ConfigLoadResult>>{
    let state=Arc::new(Mutex::new(ConfigLoadResult::Failure{reason:ConfigLoadFailureReason::MissingConfig,message:"Missing websearch config. Create .pi/websearch.json or ~/.pi/websearch.json before starting pi.".into(),source:None}));
    let current=Arc::clone(&state);
    api.on(EventKind::SessionStart,Arc::new(move|_,ctx|{let current=Arc::clone(&current);Box::pin(async move{
        let loaded=if is_provider_native_bypass(ctx.model.as_ref().map(|model|model.provider.as_str())){ConfigLoadResult::Failure{reason:ConfigLoadFailureReason::ProviderNativeBypass,message:NATIVE_BYPASS_MESSAGE.into(),source:None}}else{let home=dirs::home_dir().ok_or_else(||ExtensionFailure::new("Home directory unavailable"))?;load_websearch_config(&ctx.cwd,&home).map_err(|error|ExtensionFailure::new(error.to_string()))?};
        clear_ui(ctx);if ctx.has_ui&&let ConfigLoadResult::Failure{reason,message,..}=&loaded&&!matches!(reason,ConfigLoadFailureReason::ProviderNativeBypass){ctx.ui.notify(message,NotificationType::Error);}
        *current.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=loaded;Ok(EventResult::None)
    })}));
    api.on(EventKind::SessionShutdown,Arc::new(|_,ctx|{clear_ui(ctx);Box::pin(async{Ok(EventResult::None)})}));
    let current=Arc::clone(&state);
    api.register_command("websearch",Some("Show web search provider status".into()),None,Arc::new(move|raw_args,ctx|{
        let args=raw_args.trim_matches(|ch|matches!(ch,'\u{0009}'..='\u{000d}'|' '|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'));
        if !args.is_empty()&&args!="status"{ctx.ui.notify("Usage: /websearch status",NotificationType::Warning);return Box::pin(async{Ok(())});}
        let state=current.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state{
            ConfigLoadResult::Success{config,..}=>{let strategy=match config.strategy{RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first"};let labels=config.providers.iter().map(|entry|provider_entry_label(crate::websearch::native::provider_name(entry.config.provider),entry.config.id.as_deref(),None)).collect::<Vec<_>>().join(", ");ctx.ui.notify(&format!("Web search active: strategy={strategy}, auto={}, providers={labels}",if config.auto{"enabled"}else{"disabled"}),NotificationType::Info);}
            ConfigLoadResult::Failure{reason,message,..}=>{let bypass=matches!(reason,ConfigLoadFailureReason::ProviderNativeBypass);ctx.ui.notify(&format!("Web search {}: {message}",if bypass{"deferred"}else{"inactive"}),if bypass{NotificationType::Info}else{NotificationType::Error});}
        }
        Box::pin(async{Ok(())})
    }));state
}
