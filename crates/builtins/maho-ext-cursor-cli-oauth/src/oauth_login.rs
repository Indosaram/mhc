use maho_ai::auth::types::{Credential,OAuthCredential};
use crate::{accounts::{list_accounts,add_account,CursorCliAccountSlot,AccountSource},settings::CursorCliOauthProviderSettings};
pub const PROVIDER_ID:&str="cursor-cli-oauth";
pub const PROVIDER_NAME:&str="Cursor CLI (OAuth)";
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
