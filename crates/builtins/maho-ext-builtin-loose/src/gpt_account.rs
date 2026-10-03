use crate::account_display_name::{DisplayNameCommand, parse_display_name_command};

pub const PROVIDER_ID: &str = "chatgpt-subscription";
pub const PROVIDER_LABEL: &str = "ChatGPT Subscription OAuth";

#[derive(Debug, PartialEq, Eq)]
pub enum AccountAction {
    DisplayName(DisplayNameCommand),
    List,
    Add,
    Remove(String),
    Pin(String),
    Unpin,
    Usage,
}

pub fn parse_action(raw: &str) -> Result<AccountAction, &'static str> {
    if let Some(command) = parse_display_name_command(raw)? { return Ok(AccountAction::DisplayName(command)); }
    let mut args = raw.split_whitespace();
    Ok(match args.next().unwrap_or("list") {
        "list" => AccountAction::List,
        "add" => AccountAction::Add,
        "remove" => args.next().map_or(AccountAction::Usage, |id| AccountAction::Remove(id.into())),
        "pin" => match args.next() {
            Some("unpin") => AccountAction::Unpin,
            Some(id) => AccountAction::Pin(id.into()),
            None => AccountAction::Usage,
        },
        "unpin" => AccountAction::Unpin,
        _ => AccountAction::Usage,
    })
}

use maho_ext_api::*;
use std::sync::Arc;
pub type AccountLogin=Arc<dyn Fn(Arc<crate::oauth_login_interaction::ExtensionLoginInteraction>)->ExtensionFuture<'static,Option<maho_ai::auth::types::AccountLoginReceipt>>+Send+Sync>;
pub struct GptAccount{
    pub login:AccountLogin,
    pub open_browser:Arc<dyn Fn(&str)+Send+Sync>,
}
impl Extension for GptAccount{
    fn register(&self,api:&mut ExtensionApi){
        let login=self.login.clone();let open_browser=self.open_browser.clone();
        api.register_command("gpt-account",Some("List and manage ChatGPT Subscription OAuth accounts.".into()),Some("[add | remove <id> | pin <id> | unpin | rename <id> <display name...> | clear-name <id>]".into()),Arc::new(move|raw,ctx|{
            let login=login.clone();let open_browser=open_browser.clone();
            Box::pin(async move{
                if crate::account_display_name::account_display_name_command(ctx,PROVIDER_ID,raw).await{return Ok(());}
                let operation=async{
                    match parse_action(raw).map_err(ExtensionFailure::new)?{
                        AccountAction::List=>{
                            let accounts=ctx.model_registry.get_credential_accounts(PROVIDER_ID).await?;
                            let mut lines=vec![format!("{PROVIDER_LABEL} accounts:")];
                            if accounts.is_empty(){lines.push("  (none)".into());}
                            for account in accounts{
                                let label=account.display_name.map_or_else(||account.name.clone(),|display|format!("{display} ({})",account.name));
                                lines.push(format!("  {label} | {} | {}{}",account.source.as_str(),if account.blocked{"blocked"}else{"available"},if account.pinned{" | pinned"}else{""}));
                            }
                            ctx.ui.notify(&lines.join("\n"),NotificationType::Info);
                        }
                        AccountAction::Add=>{
                            if !ctx.has_ui{ctx.ui.notify("/gpt-account add requires an interactive UI.",NotificationType::Error);return Ok(());}
                            let interaction=Arc::new(crate::oauth_login_interaction::ExtensionLoginInteraction::new(ctx.ui.clone(),ctx.mode,PROVIDER_LABEL.into(),Some(PROVIDER_ID),open_browser));
                            let receipt=match login(interaction).await{
                                Ok(receipt)=>receipt,
                                Err(error) if error.message==crate::oauth_login_interaction::LOGIN_CANCELLED_MESSAGE=>return Ok(()),
                                Err(error)=>return Err(error),
                            };
                            ctx.ui.notify("ChatGPT Subscription OAuth account added.",NotificationType::Info);
                            crate::account_display_name::prompt_account_display_name(ctx,receipt.as_ref()).await;
                        }
                        AccountAction::Remove(name)=>{
                            ctx.model_registry.remove_credential_account(PROVIDER_ID,&name).await?;
                            ctx.ui.notify(&format!("Removed {PROVIDER_LABEL} account '{name}'."),NotificationType::Info);
                        }
                        AccountAction::Pin(name)=>{
                            ctx.model_registry.pin_credential_account(PROVIDER_ID,Some(&name)).await?;
                            ctx.ui.notify(&format!("Pinned {PROVIDER_LABEL} account '{name}'."),NotificationType::Info);
                        }
                        AccountAction::Unpin=>{
                            ctx.model_registry.pin_credential_account(PROVIDER_ID,None).await?;
                            ctx.ui.notify(&format!("Unpinned {PROVIDER_LABEL} account."),NotificationType::Info);
                        }
                        _=>ctx.ui.notify("Usage: /gpt-account [add | remove <id> | pin <id> | unpin | rename <id> <display name...> | clear-name <id>]",NotificationType::Error),
                    }
                    Ok::<(),ExtensionFailure>(())
                }.await;
                if let Err(error)=operation{ctx.ui.notify(&error.message,NotificationType::Error);}
                Ok(())
            })
        }));
    }
}
