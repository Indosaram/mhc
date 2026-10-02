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
    register_with_router(api,oauth,std::sync::Arc::new(tokio::sync::Mutex::new(crate::session_router::SessionRouter::default())));
}
pub fn register_with_router(api:&mut maho_ext_api::ExtensionApi,oauth:std::sync::Arc<crate::oauth_login::CursorCliOAuth>,router:std::sync::Arc<tokio::sync::Mutex<crate::session_router::SessionRouter>>) {
    let store=oauth.store.clone();
    api.register_command("cursor-account",Some("List and manage Cursor CLI accounts.".into()),Some("[remove <id> | pin <id> | unpin]".into()),std::sync::Arc::new(move |raw,ctx| {
        let store=store.clone();let oauth=oauth.clone();let router=router.clone();Box::pin(async move {
            let action=parse_action(raw);
            let outcome:anyhow::Result<()>=async {
                if let Some(command)=maho_ext_builtin_loose::account_display_name::parse_display_name_command(raw).map_err(anyhow::Error::msg)? {
                    store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |current|Box::pin(async move {
                        let current=current.ok_or_else(||anyhow::anyhow!("Stored provider account not found: {}",command.account_id))?;
                        let pooled=maho_ai::auth::pool::slots::PooledCredential::from(current.clone());
                        let next=maho_ai::auth::pool::slots::rename_slot_display_name(&pooled,&command.account_id,command.display_name.as_deref()).map_err(anyhow::Error::msg)?;
                        let mut current=current.into_oauth().ok_or_else(||anyhow::anyhow!("Provider account management requires an OAuth credential"))?;
                        let display=next.accounts.expect("renamed accounts").into_iter().find(|slot|slot.name==command.account_id).expect("renamed slot").display_name;
                        for slot in current.extra.get_mut("accounts").and_then(serde_json::Value::as_array_mut).ok_or_else(||anyhow::anyhow!("Stored provider account not found: {}",command.account_id))? {
                            if slot["name"]==command.account_id {
                                let object=slot.as_object_mut().expect("slot");
                                if let Some(display)=&display {object.insert("displayName".into(),serde_json::json!(display));}else {object.remove("displayName");}
                            }
                        }
                        Ok(Some(Credential::OAuth(current)))
                    })),None).await?;
                    return Ok(());
                }
                match action {
                    AccountAction::Status=> {
                        let credential=store.read(crate::oauth_login::PROVIDER_ID,None).await?.and_then(Credential::into_oauth).unwrap_or_else(empty_credential);
                        let accounts=list_accounts(&credential)?;let settings=(oauth.settings)();let session=ctx.session_manager.session_id();
                        let pin=settings.pinned_account.as_deref().or(credential.get_extra_str("pinned"));
                        let router=router.lock().await;let record=router.get_record(session);
                        let selected=record.map(|record|record.account_name.clone()).or_else(||crate::affinity::select_account(&accounts,&crate::affinity::CursorAffinityOptions {session_id:Some(session),pinned_account:pin,..Default::default()},(oauth.now)() as f64).ok().map(|slot|slot.name));
                        let mut lines=vec!["Cursor CLI (OAuth) status:".into(),"  Auth lane: file-store".into(),"  Context owner: senpi".into(),format!("  Selected account: {}",selected.as_deref().map(|name|if Some(name)==pin {format!("{name} (pinned)")}else {name.into()}).unwrap_or_else(||"none".into())),format!("  Chat id: {}",record.map_or("none (no turns yet on this session)",|record|record.chat_id.as_str())),format!("  Last model: {}",record.map_or("none",|record|record.last_model.as_str()))];
                        drop(router);
                        let environment=std::env::vars().collect();let home=std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(||ctx.cwd.clone());
                        match crate::executable::resolve_default(&environment,settings.executable_path.as_deref(),&home) {
                            Ok(executable)=> {
                                lines.push(format!("  Executable: {executable}"));
                                if let Ok(version)=crate::executable::probe_cursor_agent_version(std::path::Path::new(&executable),home.to_str().ok_or_else(||anyhow::anyhow!("HOME is not UTF-8"))?,&environment).await {
                                    lines.push(format!("  Version: {version}"));
                                    if crate::diagnostics::compare_versions(&version,crate::diagnostics::MINIMUM_KNOWN_GOOD_VERSION)==Some(std::cmp::Ordering::Less) {lines.push(format!("  WARNING: cursor-agent {version} is below the minimum known-good {}; upgrade recommended",crate::diagnostics::MINIMUM_KNOWN_GOOD_VERSION));}
                                }
                            },
                            Err(error)=>lines.push(format!("  Executable: not installed - {error}")),
                        }
                        let windows=crate::diagnostics::block_windows(&accounts,(oauth.now)() as f64);
                        if windows.is_empty() {lines.push("  Block windows: none".into());}else {
                            lines.push("  Block windows:".into());
                            for window in windows {
                                let state=match window.blocked_until {Some(at)=>format!("expires {}",chrono::DateTime::from_timestamp_millis(at as i64).ok_or_else(||anyhow::anyhow!("invalid block timestamp"))?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true)),None=>"until re-login".into()};
                                lines.push(format!("    {}: {}, {state}",window.account,window.reason));
                            }
                        }
                        if store.read("cursor",None).await?.as_ref().and_then(Credential::as_oauth).is_some_and(|credential|!credential.access.is_empty()) {lines.push("  Recommended default: the native 'cursor' provider is configured; use this Cursor CLI (OAuth) lane as the fallback when the native path misbehaves.".into());}
                        ctx.ui.notify(&lines.join("\n"),maho_ext_api::NotificationType::Info);
                    },
                    AccountAction::ImportLocal=> {
                        let environment=std::env::vars().collect();
                        let home=std::env::var_os("HOME").map(std::path::PathBuf::from).ok_or_else(||anyhow::anyhow!("No local Cursor OAuth credential found"))?;
                        let imported=crate::oauth_login::read_local_credential(&environment,&home,(oauth.now)()).await?;
                        let ui=ctx.ui.clone();let has_ui=ctx.has_ui;
                        store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |current|Box::pin(async move {
                            let current=current.and_then(Credential::into_oauth).unwrap_or_else(empty_credential);
                            let accounts=list_accounts(&current)?;
                            let name=if accounts.is_empty() {"default".into()}else {
                                let fallback=format!("account-{}",accounts.len()+1);
                                if has_ui {
                                    let answer=ui.input(&format!("Name for this account (existing: {})",accounts.iter().map(|slot|slot.name.as_str()).collect::<Vec<_>>().join(", ")),Some(&fallback),Default::default()).await.ok_or_else(||anyhow::anyhow!("Import cancelled"))?;
                                    if answer.trim().is_empty() {fallback}else {answer.trim().to_owned()}
                                }else {fallback}
                            };
                            Ok(Some(Credential::OAuth(crate::accounts::add_account(&current,crate::accounts::CursorCliAccountSlot {name,display_name:None,access:imported.access,refresh:imported.refresh,expires:imported.expires,source:crate::accounts::AccountSource::Import,blocked_until:None,block_reason:None})?)))
                        })),None).await?;
                        (oauth.persist_enabled)(true)?;
                    },
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
                        if !ctx.has_ui {anyhow::bail!("/cursor-account add requires an interactive UI.");}
                        use maho_ai::auth::types::{AuthInteraction,OAuthAuth,ProviderAuthInteraction};
                        let interaction=std::sync::Arc::new(maho_ext_builtin_loose::oauth_login_interaction::ExtensionLoginInteraction::new(ctx.ui.clone(),ctx.mode,crate::oauth_login::PROVIDER_NAME.into(),Some(crate::oauth_login::PROVIDER_ID),std::sync::Arc::new(|url| {
                            let url=url.to_owned();
                            tokio::spawn(async move {
                                let (program,args)=match std::env::consts::OS {"macos"=>("open",vec![url]),"windows"=>("rundll32",vec!["url.dll,FileProtocolHandler".into(),url]),_=>("xdg-open",vec![url])};
                                let result=tokio::process::Command::new(program).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().await;
                                if !result.is_ok_and(|status|status.success()) {eprintln!("OAuth browser launcher failed; use the displayed authorization URL.");}
                            });
                        })));
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
