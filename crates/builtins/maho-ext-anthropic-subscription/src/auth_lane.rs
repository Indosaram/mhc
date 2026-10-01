use std::{collections::BTreeMap,path::Path};
use crate::{accounts::{AccountSlot,AccountSource,env_slot_token},auth_environment::{has_request_oauth_token,merge_request_auth_environment,strip_managed_auth_environment},settings::ProviderSettings};
pub const EXPIRING_WITHIN_MS:f64=5.0*60000.0;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum TokenInjection {Ambient,OAuthSlots,ConfigDir}
pub fn resolve_effective_lane(settings:&ProviderSettings,accounts:&[AccountSlot])->TokenInjection {
    match settings.values.get("tokenInjection").and_then(serde_json::Value::as_str) {Some("ambient")=>TokenInjection::Ambient,Some("oauth-slots")=>TokenInjection::OAuthSlots,Some("config-dir")=>TokenInjection::ConfigDir,_=>if accounts.is_empty() {TokenInjection::Ambient}else {TokenInjection::OAuthSlots}}
}
pub fn request_lane(settings:&ProviderSettings,accounts:&[AccountSlot],request:Option<&BTreeMap<String,String>>)->anyhow::Result<TokenInjection> {
    let mut lane=resolve_effective_lane(settings,accounts);if lane==TokenInjection::ConfigDir&&has_request_oauth_token(request) {lane=TokenInjection::OAuthSlots;}
    if lane!=TokenInjection::Ambient&&accounts.is_empty() {anyhow::bail!("authentication_failed: No Anthropic Subscription accounts configured for the managed lane; run /login anthropic-subscription or set CLAUDE_CODE_OAUTH_TOKEN");}Ok(lane)
}
pub fn ambient_environment(host:&BTreeMap<String,String>,request:Option<&BTreeMap<String,String>>)->BTreeMap<String,String> {
    let mut environment=merge_request_auth_environment(host,request);environment.retain(|name,_|!name.starts_with("SENPI_"));environment
}
/// The caller refreshes stored slots before invoking this spawn-time projection.
pub fn prepared_environment(environment:&BTreeMap<String,String>,lane:TokenInjection,slot:&AccountSlot,agent_dir:&Path,now_ms:u64)->anyhow::Result<BTreeMap<String,String>> {
    let access=if slot.source==AccountSource::Env {env_slot_token(&|name|environment.get(name).cloned(),&slot.name)}else {Some(slot.access.clone())}.filter(|token|!token.is_empty()).ok_or_else(||anyhow::anyhow!("authentication_failed: selected OAuth token is unavailable"))?;
    let mut child=strip_managed_auth_environment(environment);
    match lane {TokenInjection::OAuthSlots=>{child.insert("CLAUDE_CODE_OAUTH_TOKEN".into(),access);},TokenInjection::ConfigDir=>{let directory=crate::config_dir_credentials::write_config_dir_credential(agent_dir,slot,&access,now_ms)?;child.insert("CLAUDE_CONFIG_DIR".into(),directory.to_string_lossy().into_owned());},TokenInjection::Ambient=>anyhow::bail!("ambient lane does not prepare a managed slot")};Ok(child)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn settings(lane:&str)->ProviderSettings {crate::settings::load(&serde_json::json!({"anthropicSubscriptionProvider":{"tokenInjection":lane}}),&serde_json::Value::Null,&BTreeMap::new())}
    fn slot()->AccountSlot {AccountSlot {name:"env".into(),display_name:None,refresh:String::new(),access:String::new(),expires:0.0,source:AccountSource::Env,blocked_until:None,block_reason:None}}
    #[test]
    fn managed_empty_pool_refuses_and_request_token_changes_config_root_lane() {
        assert!(request_lane(&settings("oauth-slots"),&[],None).is_err());assert_eq!(request_lane(&settings("ambient"),&[],None).expect("ambient"),TokenInjection::Ambient);let request=[("CLAUDE_CODE_OAUTH_TOKEN".into(),"synthetic".into())].into();assert_eq!(request_lane(&settings("config-dir"),&[slot()],Some(&request)).expect("request"),TokenInjection::OAuthSlots);
    }
    #[test]
    fn managed_spawn_uses_only_selected_token_and_ambient_keeps_host_auth() {
        let environment:BTreeMap<_,_>=[("CLAUDE_CODE_OAUTH_TOKEN".into(),"synthetic".into()),("CLAUDE_CODE_OAUTH_TOKEN_2".into(),"other".into()),("ANTHROPIC_API_KEY".into(),"host".into()),("SENPI_PRIVATE".into(),"marker".into()),("PATH".into(),"/bin".into())].into();let directory=tempfile::tempdir().expect("dir");let child=prepared_environment(&environment,TokenInjection::OAuthSlots,&slot(),directory.path(),0).expect("prepare");assert_eq!(child.len(),2);assert_eq!(child["CLAUDE_CODE_OAUTH_TOKEN"],"synthetic");let ambient=ambient_environment(&environment,None);assert!(ambient.contains_key("ANTHROPIC_API_KEY"));assert!(!ambient.contains_key("SENPI_PRIVATE"));
    }
}
