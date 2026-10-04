use maho_ai::auth::types::Credential;
use crate::{oauth_login::{assess_configuration,ConfigurationOutcome},settings::CursorCliOauthProviderSettings};
pub async fn refresh_catalog<F,Fut>(agent_dir:&std::path::Path,settings:&CursorCliOauthProviderSettings,current:Option<&Credential>,executable:&std::path::Path,environment:&std::collections::BTreeMap<String,String>,now:f64,resolve:F)->anyhow::Result<Option<Vec<maho_ext_api::types::ProviderModelConfig>>>
where F:FnOnce()->Fut,Fut:std::future::Future<Output=anyhow::Result<()>> {
    if settings.explicitly_disabled {return Ok(None);}
    let resolved=resolve().await;
    let outcome=assess_configuration(settings,current,||resolved)?;
    let ConfigurationOutcome::Configured(accounts)=outcome else {return Ok(None);};
    let Some(account)=accounts.iter().find(|a|Some(a.name.as_str())==settings.pinned_account.as_deref()).or_else(||accounts.first()) else {return Ok(None);};
    let models=crate::models::resolve_catalog(agent_dir,now,Some(settings.model_catalog_ttl_hours),||async {
        let directory=tempfile::Builder::new().prefix("senpi-cursor-models-").tempdir()?;let stdout=directory.path().join("stdout.txt");
        let result=crate::home_store::run_in_account_home(agent_dir,account,|home|async move {
            crate::models_probe::run_models_probe(executable,&stdout,15000,home.home.to_str().ok_or_else(||anyhow::anyhow!("account HOME is not UTF-8"))?,environment).await?;
            Ok(std::fs::read_to_string(stdout)?)
        },|_|{}).await?;
        Ok(result.result)
    }).await;
    Ok(Some(models))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn disabled_and_empty_accounts_never_probe() {
        let directory=tempfile::tempdir().expect("directory");let mut settings=CursorCliOauthProviderSettings {explicitly_disabled:true,..Default::default()};let environment=Default::default();
        assert!(refresh_catalog(directory.path(),&settings,None,std::path::Path::new("/missing"),&environment,1000.0,||async {panic!("disabled resolver")}).await.expect("disabled").is_none());
        settings.explicitly_disabled=false;settings.enabled=true;
        assert!(refresh_catalog(directory.path(),&settings,None,std::path::Path::new("/missing"),&environment,1000.0,||async {Ok(())}).await.expect("empty").is_none());
    }
    #[tokio::test]
    async fn probes_in_pinned_account_home() {
        use std::os::unix::fs::PermissionsExt;
        let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("cursor-agent");
        std::fs::write(&executable,"#!/bin/sh\ncase \"$HOME\" in */accounts/second/home) ;; *) exit 9;; esac\n[ -f \"$HOME/.cursor/auth.json\" ] || exit 8\nprintf 'model-a - Model A\\n'\n").expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let mut credential=crate::accounts::empty_credential();
        for name in ["default","second"] {credential=crate::accounts::add_account(&credential,crate::accounts::CursorCliAccountSlot {name:name.into(),display_name:None,access:"fake-access".into(),refresh:"fake-refresh".into(),expires:100000.0,source:crate::accounts::AccountSource::Login,blocked_until:None,block_reason:None}).expect("slot");}
        let settings=CursorCliOauthProviderSettings {pinned_account:Some("second".into()),..Default::default()};
        let models=refresh_catalog(directory.path(),&settings,Some(&Credential::OAuth(credential)),&executable,&Default::default(),1000.0,||async {Ok(())}).await.expect("refresh").expect("catalog");
        assert_eq!(models[0].id,"model-a");
    }
}
