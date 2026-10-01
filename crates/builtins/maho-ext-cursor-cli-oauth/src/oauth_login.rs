use maho_ai::auth::types::{Credential,OAuthCredential};
use crate::{accounts::{list_accounts,add_account,CursorCliAccountSlot,AccountSource},settings::CursorCliOauthProviderSettings};
pub const PROVIDER_ID:&str="cursor-cli-oauth";
pub const PROVIDER_NAME:&str="Cursor CLI (OAuth)";
pub type ExecutableCheck=std::sync::Arc<dyn Fn(&CursorCliOauthProviderSettings)->anyhow::Result<()>+Send+Sync>;
pub type AcknowledgementWriter=std::sync::Arc<dyn Fn(&str)->anyhow::Result<()>+Send+Sync>;
pub struct CursorCliOAuth {
    pub store:std::sync::Arc<dyn maho_ai::auth::types::CredentialStore>,
    pub flow:std::sync::Arc<dyn maho_ai::auth::types::OAuthAuth>,
    pub settings:std::sync::Arc<dyn Fn()->CursorCliOauthProviderSettings+Send+Sync>,
    pub resolve:ExecutableCheck,
    pub persist_acknowledgement:AcknowledgementWriter,
    pub persist_enabled:std::sync::Arc<dyn Fn(bool)->anyhow::Result<()>+Send+Sync>,
    pub now:std::sync::Arc<dyn Fn()->i64+Send+Sync>,
}
#[async_trait::async_trait]
impl maho_ai::auth::types::OAuthAuth for CursorCliOAuth {
    fn name(&self)->&str {PROVIDER_NAME}
    fn is_subscription(&self)->bool {true}
    async fn login(&self,interaction:&maho_ai::auth::types::ProviderAuthInteraction)->anyhow::Result<OAuthCredential> {
        use maho_ai::auth::types::{AuthPrompt,AuthPromptKind};
        let stored=self.store.read(PROVIDER_ID,None).await?;
        let current=stored.and_then(Credential::into_oauth).filter(|c|c.extra.get("accounts").is_some_and(serde_json::Value::is_array)).unwrap_or_else(crate::accounts::empty_credential);
        let existing=list_accounts(&current)?;let logged=self.flow.login(interaction).await?;
        let name=if existing.is_empty() {"default".into()} else {
            let fallback=format!("account-{}",existing.len()+1);
            let answer=interaction.prompt(AuthPrompt {kind:AuthPromptKind::Text {message:format!("Name for this account (existing: {})",existing.iter().map(|s|s.name.as_str()).collect::<Vec<_>>().join(", ")),placeholder:Some(fallback.clone())},signal:Some(interaction.signal.clone())}).await?;
            if answer.trim().is_empty() {fallback} else {answer.trim().into()}
        };
        let explanation=format!("{} Plan mode (planning only, no tool execution) is available via the executionMode setting.\nType \"yes\" to acknowledge and allow unattended Cursor CLI tool execution; anything else leaves it off.",crate::guardrails::NO_APPROVAL_EXPLANATION);
        let answer=interaction.prompt(AuthPrompt {kind:AuthPromptKind::Text {message:explanation,placeholder:None},signal:Some(interaction.signal.clone())}).await?;
        if ["yes","y"].contains(&answer.trim().to_lowercase().as_str()) {
            let at=chrono::DateTime::from_timestamp_millis((self.now)()).ok_or_else(||anyhow::anyhow!("invalid acknowledgement timestamp"))?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
            (self.persist_acknowledgement)(&at)?;
        }
        let credential=add_account(&current,CursorCliAccountSlot {name,display_name:None,access:logged.access,refresh:logged.refresh,expires:logged.expires,source:AccountSource::Login,blocked_until:None,block_reason:None})?;
        (self.persist_enabled)(true)?;Ok(credential)
    }
    async fn refresh(&self,credential:&OAuthCredential,signal:&maho_ai::utils::abort::AbortSignal)->anyhow::Result<OAuthCredential> {
        if !credential.extra.get("accounts").is_some_and(serde_json::Value::is_array) {return Ok(credential.clone());}
        let mut accounts=list_accounts(credential)?;
        for slot in &mut accounts {
            if ((self.now)() as f64)<slot.expires {continue;}
            let refreshed=self.flow.refresh(&OAuthCredential::new(&slot.access,&slot.refresh,slot.expires),signal).await?;
            slot.access=refreshed.access;slot.refresh=refreshed.refresh;slot.expires=refreshed.expires;
        }
        let mut result=credential.clone();result.access=crate::accounts::SENTINEL_TOKEN.into();result.refresh=crate::accounts::SENTINEL_TOKEN.into();result.expires=crate::accounts::SENTINEL_EXPIRES;
        result.extra.insert("accounts".into(),serde_json::to_value(accounts)?);Ok(result)
    }
    async fn check(&self,_ctx:&dyn maho_ai::auth::types::AuthContext,credential:Option<&OAuthCredential>,_signal:&maho_ai::utils::abort::AbortSignal)->anyhow::Result<Option<maho_ai::auth::types::AuthCheck>> {
        let current=self.store.read(PROVIDER_ID,None).await?;let settings=(self.settings)();
        let source=match assess_configuration(&settings,current.as_ref(),||(self.resolve)(&settings))? {
            ConfigurationOutcome::Disabled|ConfigurationOutcome::NoAccounts=>return Ok(None),
            ConfigurationOutcome::NotInstalled(error)=>{if credential.is_none() {return Ok(None);}format!("cursor-agent not installed: {}",error.trim_start_matches("Cursor CLI is not installed. "))},
            ConfigurationOutcome::Configured(accounts)=>format!("configured (file-store, {} accounts)",accounts.len()),
        };
        Ok(Some(maho_ai::auth::types::AuthCheck {source:Some(source),auth_type:maho_ai::auth::types::AuthType::OAuth}))
    }
    async fn to_auth(&self,_credential:&OAuthCredential)->anyhow::Result<maho_ai::auth::types::ModelAuth> {
        Ok(maho_ai::auth::types::ModelAuth {api_key:Some(crate::accounts::SENTINEL_TOKEN.into()),..Default::default()})
    }
}
pub enum ConfigurationOutcome { Disabled,NotInstalled(String),NoAccounts,Configured(Vec<CursorCliAccountSlot>) }
pub fn lane_enabled(settings:&CursorCliOauthProviderSettings,stored_count:usize)->bool {
    !settings.explicitly_disabled&&(settings.enabled||stored_count>0)
}
pub fn assess_configuration(settings:&CursorCliOauthProviderSettings,current:Option<&Credential>,resolve:impl FnOnce()->anyhow::Result<()>)->anyhow::Result<ConfigurationOutcome> {
    if settings.explicitly_disabled {return Ok(ConfigurationOutcome::Disabled);}
    if let Err(error)=resolve() {return Ok(ConfigurationOutcome::NotInstalled(error.to_string()));}
    let accounts=match current {Some(Credential::OAuth(credential))=>list_accounts(credential)?.into_iter().filter(|s|!s.access.trim().is_empty()&&!s.refresh.trim().is_empty()&&s.expires.is_finite()).collect(),_=>Vec::new()};
    if !lane_enabled(settings,accounts.len()) {return Ok(ConfigurationOutcome::Disabled);}
    if accounts.is_empty() {return Ok(ConfigurationOutcome::NoAccounts);}
    Ok(ConfigurationOutcome::Configured(accounts))
}
pub fn import_native_credential(current:&OAuthCredential,native:Option<&Credential>)->anyhow::Result<OAuthCredential> {
    let Some(Credential::OAuth(native))=native else {anyhow::bail!("No stored native Cursor OAuth credential found");};
    if native.access.is_empty()||native.refresh.is_empty()||!native.expires.is_finite() {anyhow::bail!("No stored native Cursor OAuth credential found");}
    let accounts=list_accounts(current)?;
    let name=std::iter::once("native".to_owned()).chain((2..=10000).map(|i|format!("native-{i}"))).find(|name|!accounts.iter().any(|s|s.name==*name)).ok_or_else(||anyhow::anyhow!("Could not allocate a Cursor CLI OAuth native account name"))?;
    add_account(current,CursorCliAccountSlot {name,display_name:None,access:native.access.clone(),refresh:native.refresh.clone(),expires:native.expires,source:AccountSource::Import,blocked_until:None,block_reason:None})
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::empty_credential;
    #[test]
    fn native_import_allocates_names_preserves_sentinel_and_source() {
        let native=Credential::OAuth(OAuthCredential::new("access","refresh",1000.0));
        let first=import_native_credential(&empty_credential(),Some(&native)).expect("import");
        let second=import_native_credential(&first,Some(&native)).expect("duplicate import");let accounts=list_accounts(&second).expect("accounts");
        assert_eq!(accounts[0].name,"native");assert_eq!(accounts[1].name,"native-2");assert_eq!(accounts[0].source,AccountSource::Import);crate::accounts::assert_sentinel_invariant(&second).expect("sentinel");
        assert!(import_native_credential(&first,None).is_err());
    }
    #[test]
    fn explicit_slots_enable_lane_but_kill_switch_wins() {
        let mut settings=CursorCliOauthProviderSettings::default();assert!(!lane_enabled(&settings,0));assert!(lane_enabled(&settings,1));settings.explicitly_disabled=true;assert!(!lane_enabled(&settings,1));
        assert!(matches!(assess_configuration(&settings,None,||panic!("disabled must not resolve")).expect("disabled"),ConfigurationOutcome::Disabled));
        settings.explicitly_disabled=false;settings.enabled=true;
        assert!(matches!(assess_configuration(&settings,None,||Ok(())).expect("empty"),ConfigurationOutcome::NoAccounts));
        assert!(matches!(assess_configuration(&settings,None,||anyhow::bail!("missing")).expect("missing"),ConfigurationOutcome::NotInstalled(_)));
    }
}
