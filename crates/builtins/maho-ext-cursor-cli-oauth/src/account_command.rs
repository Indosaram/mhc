use maho_ai::auth::types::{Credential,CredentialStore};
use crate::accounts::{empty_credential,list_accounts,pin_account,remove_account};
#[derive(Debug,PartialEq,Eq)]
pub enum AccountAction {List,Add,Remove(String),Pin(String),Unpin,ImportLocal,ImportNative,Acknowledge,Status,Usage}
pub fn parse_action(raw:&str)->AccountAction {
    let mut args=raw.split_whitespace();let action=args.next().unwrap_or("list");let name=args.next();
    match (action,name) {
        ("list",_)=>AccountAction::List,("add",_)=>AccountAction::Add,("remove",Some(name))=>AccountAction::Remove(name.into()),
        ("pin",Some(name))=>AccountAction::Pin(name.into()),("unpin",_)=>AccountAction::Unpin,("import",None|Some("local"))=>AccountAction::ImportLocal,
        ("import",Some("native"))=>AccountAction::ImportNative,("acknowledge",_)=>AccountAction::Acknowledge,("status",_)=>AccountAction::Status,_=>AccountAction::Usage,
    }
}
pub async fn mutate_account(store:&dyn CredentialStore,action:AccountAction)->anyhow::Result<Option<Credential>> {
    store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |current|Box::pin(async move {
        let credential=current.as_ref().and_then(Credential::as_oauth).cloned().unwrap_or_else(empty_credential);
        match action {
            AccountAction::Remove(name)=> {
                if !list_accounts(&credential)?.iter().any(|a|a.name==name) {anyhow::bail!("Cursor CLI (OAuth) account '{name}' does not exist.");}
                Ok(Some(Credential::OAuth(remove_account(&credential,&name)?)))
            },
            AccountAction::Pin(name)=> {
                if !list_accounts(&credential)?.iter().any(|a|a.name==name) {anyhow::bail!("Cursor CLI (OAuth) account '{name}' does not exist.");}
                Ok(Some(Credential::OAuth(pin_account(&credential,&name)?)))
            },
            AccountAction::Unpin=>{let mut unpinned=credential;unpinned.extra.remove("pinned");Ok(Some(Credential::OAuth(unpinned)))},
            _=>Ok(current),
        }
    })),None).await
}
pub fn register(api:&mut maho_ext_api::ExtensionApi,oauth:std::sync::Arc<crate::oauth_login::CursorCliOAuth>) {
    let store=oauth.store.clone();
    api.register_command("cursor-account",Some("List and manage Cursor CLI accounts.".into()),Some("[remove <id> | pin <id> | unpin]".into()),std::sync::Arc::new(move |raw,ctx| {
        let store=store.clone();let oauth=oauth.clone();Box::pin(async move {
            let action=parse_action(raw);
            let outcome:anyhow::Result<()>=async {
                if let Some(command)=maho_ext_builtin_loose::account_display_name::parse_display_name_command(raw).map_err(anyhow::Error::msg)? {
                    store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |current|Box::pin(async move {
                        let current=current.ok_or_else(||anyhow::anyhow!("Stored provider account not found: {}",command.account_id))?;
                        let pooled=maho_ai::auth::pool::slots::PooledCredential::from(current);
                        let next=maho_ai::auth::pool::slots::rename_slot_display_name(&pooled,&command.account_id,command.display_name.as_deref()).map_err(anyhow::Error::msg)?;
                        Ok(Some(next.to_stored_credential()))
                    })),None).await?;
                    return Ok(());
                }
                match action {
                    AccountAction::ImportNative=> {
                        let native=store.read("cursor",None).await?;
                        store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |current|Box::pin(async move {
                            let current=current.and_then(Credential::into_oauth).unwrap_or_else(empty_credential);
                            Ok(Some(Credential::OAuth(crate::oauth_login::import_native_credential(&current,native.as_ref())?)))
                        })),None).await?;
                        (oauth.persist_enabled)(true)?;
                    },
                    AccountAction::Acknowledge=> {
                        if !ctx.has_ui { anyhow::bail!("/cursor-account acknowledge requires an interactive UI."); }
                        let answer=ctx.ui.input(crate::guardrails::NO_APPROVAL_EXPLANATION,Some("yes"),Default::default()).await;
                        if answer.as_deref().is_some_and(|answer|["yes","y"].contains(&answer.trim().to_lowercase().as_str())) {
                            let at=chrono::DateTime::from_timestamp_millis((oauth.now)()).ok_or_else(||anyhow::anyhow!("invalid acknowledgement timestamp"))?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
                            (oauth.persist_acknowledgement)(&at)?;
                        }
                    },
                    AccountAction::Add=> {
                        use maho_ai::auth::types::{AuthInteraction,OAuthAuth,ProviderAuthInteraction};
                        let interaction=std::sync::Arc::new(maho_ext_builtin_loose::oauth_login_interaction::ExtensionLoginInteraction::new(ctx.ui.clone(),ctx.mode,crate::oauth_login::PROVIDER_NAME.into(),Some(crate::oauth_login::PROVIDER_ID),std::sync::Arc::new(|_| {})));
                        let interaction=ProviderAuthInteraction::new(interaction.signal().expect("login signal"),interaction);
                        let credential=oauth.login(&interaction).await?;
                        store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await?;
                    },
                    AccountAction::List=> {
                        let stored=store.read(crate::oauth_login::PROVIDER_ID,None).await?;
                        let credential=stored.and_then(Credential::into_oauth).unwrap_or_else(empty_credential);
                        let accounts=list_accounts(&credential)?;
                        let message=if accounts.is_empty() {"No Cursor CLI accounts configured.".into()}else {accounts.iter().map(|slot|format!("{}{}",slot.name,if credential.get_extra_str("pinned")==Some(slot.name.as_str()) {" (pinned)"}else {""})).collect::<Vec<_>>().join("\n")};
                        ctx.ui.notify(&message,maho_ext_api::NotificationType::Info);
                    },
                    AccountAction::Remove(_)|AccountAction::Pin(_)|AccountAction::Unpin=> {mutate_account(store.as_ref(),action).await?;},
                    _=>ctx.ui.notify("Usage: /cursor-account [remove <id> | pin <id> | unpin]",maho_ext_api::NotificationType::Warning),
                }Ok(())
            }.await;
            if let Err(error)=outcome {ctx.ui.notify(&error.to_string(),maho_ext_api::NotificationType::Error);}Ok(())
        })
    }));
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_route_actions() {
        assert_eq!(parse_action(""),AccountAction::List);assert_eq!(parse_action(" pin work "),AccountAction::Pin("work".into()));assert_eq!(parse_action("import native"),AccountAction::ImportNative);assert_eq!(parse_action("import invalid"),AccountAction::Usage);assert_eq!(parse_action("remove"),AccountAction::Usage);
    }
}
