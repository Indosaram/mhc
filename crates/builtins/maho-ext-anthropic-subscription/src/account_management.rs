use std::{collections::BTreeMap, sync::Arc};
use maho_ai::auth::types::{Credential, CredentialStore, OAuthCredential};
use crate::accounts::{AccountSource, empty_credential, list_accounts, pin_account, remove_account};
pub const ANTHROPIC_SUBSCRIPTION_PROVIDER_ID: &str = "anthropic-subscription";
#[derive(Debug, PartialEq)]
pub struct ProviderAccountSummary { pub name:String, pub display_name:Option<String>, pub source:AccountSource, pub blocked:bool, pub pinned:bool }
fn assert_managed_provider(provider:&str)->anyhow::Result<()> {
    if provider!=ANTHROPIC_SUBSCRIPTION_PROVIDER_ID {anyhow::bail!("Provider account management is unavailable for: {provider}");} Ok(())
}
fn credential_from(credential:Option<Credential>)->anyhow::Result<OAuthCredential> {
    match credential {None=>Ok(empty_credential()),Some(Credential::OAuth(credential))=>Ok(credential),Some(Credential::ApiKey(_))=>anyhow::bail!("Provider account management requires an OAuth credential")}
}
pub async fn get_provider_accounts(store:&dyn CredentialStore,provider:&str,environment:&BTreeMap<String,String>,now:f64)->anyhow::Result<Vec<ProviderAccountSummary>> {
    assert_managed_provider(provider)?;let credential=credential_from(store.read(provider,None).await?)?;
    Ok(list_accounts(&credential,Some(&|key|environment.get(key).cloned()))?.into_iter().map(|account| {
        let display_name=maho_ai::auth::pool::slots::account_display_name(account.display_name.as_deref());
        ProviderAccountSummary {blocked:account.block_reason.as_deref()==Some("auth_error")||account.blocked_until.is_some_and(|until|until>now),pinned:credential.get_extra_str("pinned")==Some(&account.name),name:account.name,display_name,source:account.source}
    }).collect())
}
pub async fn pin_provider_account(store:&dyn CredentialStore,provider:&str,name:Option<&str>,environment:Arc<BTreeMap<String,String>>)->anyhow::Result<()> {
    assert_managed_provider(provider)?;let name=name.map(str::to_owned);
    store.modify(provider,Box::new(move |current|Box::pin(async move {
        let mut credential=credential_from(current.clone())?;
        if let Some(name)=name {
            if !list_accounts(&credential,Some(&|key|environment.get(key).cloned()))?.iter().any(|account|account.name==name) {anyhow::bail!("Provider account not found: {name}");}
            credential=pin_account(&credential,&name);
        } else if credential.extra.remove("pinned").is_none() {return Ok(current);}
        Ok(Some(Credential::OAuth(credential)))
    })),None).await?;
    crate::account_events::emit_provider_accounts_changed(provider);Ok(())
}
pub async fn remove_provider_account(store:&dyn CredentialStore,provider:&str,name:&str,environment:Arc<BTreeMap<String,String>>)->anyhow::Result<()> {
    assert_managed_provider(provider)?;let name=name.to_owned();
    store.modify(provider,Box::new(move |current|Box::pin(async move {
        let credential=credential_from(current)?;
        let stored=credential.extra.get("accounts").and_then(serde_json::Value::as_array).is_some_and(|accounts|accounts.iter().any(|account|account["name"]==name));
        if !stored {
            if list_accounts(&credential,Some(&|key|environment.get(key).cloned()))?.iter().any(|account|account.name==name&&account.source==AccountSource::Env) {anyhow::bail!("Environment provider account cannot be removed: {name}");}
            anyhow::bail!("Provider account not found: {name}");
        }
        Ok(Some(Credential::OAuth(remove_account(&credential,&name)?)))
    })),None).await?;
    crate::account_events::emit_provider_accounts_changed(provider);Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountSlot,add_account};
    use maho_ai::auth::credential_store::InMemoryCredentialStore;
    async fn seeded()->InMemoryCredentialStore {
        let store=InMemoryCredentialStore::new();let credential=add_account(&empty_credential(),AccountSlot {name:"primary".into(),display_name:Some("  Main  ".into()),access:String::new(),refresh:String::new(),expires:0.0,source:AccountSource::Login,blocked_until:Some(100.0),block_reason:None}).expect("slot");
        store.modify(ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("seed");store
    }
    #[tokio::test]
    async fn summaries_include_environment_pin_and_live_block_state() {
        let store=seeded().await;let environment:Arc<BTreeMap<String,String>>=Arc::new([(String::from("CLAUDE_CODE_OAUTH_TOKEN"),String::from("fixture"))].into());
        pin_provider_account(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,Some("env"),environment.clone()).await.expect("pin");
        let summaries=get_provider_accounts(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,&environment,50.0).await.expect("summaries");assert_eq!(summaries.len(),2);assert!(summaries[0].blocked);assert_eq!(summaries[0].display_name.as_deref(),Some("Main"));assert!(summaries[1].pinned);
        pin_provider_account(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,None,environment.clone()).await.expect("unpin");assert!(!get_provider_accounts(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,&environment,101.0).await.expect("summaries")[0].blocked);
    }
    #[tokio::test]
    async fn removal_refuses_environment_and_missing_slots_and_unpins_stored_slot() {
        let store=seeded().await;let environment:Arc<BTreeMap<String,String>>=Arc::new([(String::from("CLAUDE_CODE_OAUTH_TOKEN"),String::from("fixture"))].into());
        for name in ["env","missing"] {assert!(remove_provider_account(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,name,environment.clone()).await.is_err());}
        pin_provider_account(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,Some("primary"),environment.clone()).await.expect("pin");remove_provider_account(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,"primary",environment.clone()).await.expect("remove");
        let summaries=get_provider_accounts(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,&environment,0.0).await.expect("summaries");assert_eq!(summaries.len(),1);assert!(!summaries[0].pinned);
    }
    #[tokio::test]
    async fn unsupported_provider_and_api_key_credentials_are_rejected() {
        let store=InMemoryCredentialStore::new();assert!(get_provider_accounts(&store,"other",&BTreeMap::new(),0.0).await.is_err());
        store.modify(ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,Box::new(|_|Box::pin(async {Ok(Some(Credential::ApiKey(maho_ai::auth::types::ApiKeyCredential {key:Some("fixture".into()),env:None})))})),None).await.expect("seed");
        assert!(get_provider_accounts(&store,ANTHROPIC_SUBSCRIPTION_PROVIDER_ID,&BTreeMap::new(),0.0).await.is_err());
    }
}
