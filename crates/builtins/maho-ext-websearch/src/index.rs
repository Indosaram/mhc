use crate::websearch::{types::{ConfigLoadResult,ConfigLoadFailureReason,RoutingStrategy},search::provider_entry_label};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum StatusLevel { Info,Warning,Error }
pub fn status_message(raw_args:&str,state:&ConfigLoadResult)->(String,StatusLevel) {
    let args=raw_args.trim_matches(|character:char|character.is_whitespace() || character=='\u{feff}');
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
    #[test] fn status_reason_controls_notification_level() { let state=ConfigLoadResult::Err{reason:ConfigLoadFailureReason::ProviderNativeBypass,message:"native".into(),source:None}; assert_eq!(status_message("status",&state).1,StatusLevel::Info); assert_eq!(status_message("reload",&state).1,StatusLevel::Warning); let state=ConfigLoadResult::Err{reason:ConfigLoadFailureReason::MissingConfig,message:"missing".into(),source:None}; assert_eq!(status_message("",&state).1,StatusLevel::Error); }
}
