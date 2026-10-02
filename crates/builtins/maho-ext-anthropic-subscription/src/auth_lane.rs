use std::{collections::BTreeMap,path::Path};
use crate::{accounts::{AccountSlot,AccountSource,env_slot_token},auth_environment::{has_request_oauth_token,merge_request_auth_environment,strip_managed_auth_environment},settings::ProviderSettings};
pub const EXPIRING_WITHIN_MS:f64=5.0*60000.0;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum TokenInjection {Ambient,OAuthSlots,ConfigDir}
pub const PROVIDER_ID:&str="anthropic-subscription";
pub struct ManagedPool {pub accounts:Vec<AccountSlot>,pub environment:BTreeMap<String,String>,pub lane:TokenInjection,pub pinned_account:Option<String>}
impl ManagedPool {
    pub async fn refresh_selected<F,Fut>(&self,store:&dyn maho_ai::auth::types::CredentialStore,selected:&mut AccountSlot,now:f64,signal:maho_ai::utils::abort::AbortSignal,refresh:F)->anyhow::Result<()>
    where F:FnOnce(String,maho_ai::utils::abort::AbortSignal)->Fut+Send+'static,Fut:std::future::Future<Output=anyhow::Result<maho_ai::auth::types::OAuthCredential>>+Send+'static {
        if selected.source==AccountSource::Env||now<selected.expires-EXPIRING_WITHIN_MS {return Ok(());}
        let refreshed=crate::accounts::refresh_slot(store,PROVIDER_ID,&selected.name,refresh,signal,move |expires|now>=expires-EXPIRING_WITHIN_MS).await;
        let update=match refreshed {
            Ok(credential)=>{let empty=crate::accounts::empty_credential();crate::accounts::list_accounts(credential.as_ref().and_then(|value|value.as_oauth()).unwrap_or(&empty),Some(&|name|self.environment.get(name).cloned()))?.into_iter().find(|candidate|candidate.name==selected.name).ok_or_else(||anyhow::anyhow!("selected account disappeared during refresh"))},
            Err(error)=>Err(error),
        };
        match update {Ok(updated)=>{*selected=updated;Ok(())},Err(error)=>{let detail=error.to_string();let classification=crate::errors::classify_sdk_error(&serde_json::json!(detail));anyhow::bail!("{}: {detail}",if classification.kind==crate::errors::SdkErrorKind::Other&&classification.retryable {"server_error"}else {"authentication_failed"});}}
    }
}
pub async fn managed_pool(store:&dyn maho_ai::auth::types::CredentialStore,settings:&ProviderSettings,host:&BTreeMap<String,String>,request:Option<&BTreeMap<String,String>>)->anyhow::Result<Option<ManagedPool>> {
    use maho_ai::auth::types::Credential;
    let mut credential=store.read(PROVIDER_ID,None).await?;let environment=merge_request_auth_environment(host,request);let read_env=|name:&str|environment.get(name).cloned();let empty=crate::accounts::empty_credential();
    let mut accounts=crate::accounts::list_accounts(credential.as_ref().and_then(|value|value.as_oauth()).unwrap_or(&empty),Some(&read_env))?;
    if credential.is_none()&&!accounts.is_empty() {credential=store.modify(PROVIDER_ID,Box::new(|_|Box::pin(async {Ok(Some(Credential::OAuth(crate::accounts::empty_credential())))})),None).await?;accounts=crate::accounts::list_accounts(credential.as_ref().and_then(|value|value.as_oauth()).unwrap_or(&empty),Some(&read_env))?;}
    let lane=request_lane(settings,&accounts,request)?;if lane==TokenInjection::Ambient {return Ok(None);}
    let pinned_account=settings.values.get("pinnedAccount").and_then(serde_json::Value::as_str).or_else(||credential.as_ref().and_then(|value|value.as_oauth()).and_then(|value|value.get_extra_str("pinned"))).map(str::to_owned);
    Ok(Some(ManagedPool {accounts,environment,lane,pinned_account}))
}
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
    #[tokio::test]
    async fn refresh_at_expiry_margin_updates_selected_slot_before_spawn() {
        use maho_ai::{auth::{credential_store::InMemoryCredentialStore,types::{CredentialStore,Credential,OAuthCredential}},utils::abort::AbortController};
        let store=InMemoryCredentialStore::new();let mut selected=slot();selected.name="work".into();selected.source=AccountSource::Login;selected.access="expired".into();selected.refresh="synthetic-refresh".into();selected.expires=EXPIRING_WITHIN_MS;let credential=crate::accounts::add_account(&crate::accounts::empty_credential(),selected.clone()).expect("account");store.modify(PROVIDER_ID,Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("seed");let pool=ManagedPool {accounts:vec![selected.clone()],environment:BTreeMap::new(),lane:TokenInjection::OAuthSlots,pinned_account:None};pool.refresh_selected(&store,&mut selected,0.0,AbortController::new().signal(),|_,_|async {Ok(OAuthCredential::new("fresh","fresh-refresh",1000000.0))}).await.expect("refresh");assert_eq!(selected.access,"fresh");let directory=tempfile::tempdir().expect("dir");assert_eq!(prepared_environment(&pool.environment,pool.lane,&selected,directory.path(),0).expect("spawn environment")["CLAUDE_CODE_OAUTH_TOKEN"],"fresh");
    }
    #[tokio::test]
    async fn env_only_discovery_seeds_sentinel_and_settings_pin_wins() {
        use maho_ai::auth::{credential_store::InMemoryCredentialStore,types::CredentialStore};
        let store=InMemoryCredentialStore::new();let environment=[("CLAUDE_CODE_OAUTH_TOKEN".into(),"synthetic".into())].into();let settings=crate::settings::load(&serde_json::json!({"anthropicSubscriptionProvider":{"pinnedAccount":"env"}}),&serde_json::Value::Null,&BTreeMap::new());let pool=managed_pool(&store,&settings,&environment,None).await.expect("pool").expect("managed");assert_eq!(pool.lane,TokenInjection::OAuthSlots);assert_eq!(pool.accounts.len(),1);assert_eq!(pool.pinned_account.as_deref(),Some("env"));let credential=store.read(PROVIDER_ID,None).await.expect("read").expect("credential").into_oauth().expect("oauth");crate::accounts::assert_sentinel_invariant(&credential).expect("sentinel");assert!(crate::accounts::list_accounts(&credential,None).expect("stored accounts").is_empty());
    }
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
