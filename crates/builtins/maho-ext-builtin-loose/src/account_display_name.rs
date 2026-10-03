#[derive(Debug, PartialEq, Eq)]
pub struct DisplayNameCommand {
    pub account_id: String,
    pub display_name: Option<String>,
}

pub fn parse_display_name_command(
    raw: &str,
) -> Result<Option<DisplayNameCommand>, &'static str> {
    let raw = raw.trim();
    let mut words = raw.split_whitespace();
    let action = words.next().unwrap_or("");
    if action != "rename" && action != "clear-name" {
        return Ok(None);
    }
    let name = words
        .next()
        .ok_or("Usage: rename <id> <display name...> or clear-name <id>")?;
    if action == "clear-name" {
        if words.next().is_some() {
            return Err("Usage: rename <id> <display name...> or clear-name <id>");
        }
        return Ok(Some(DisplayNameCommand {
            account_id: name.into(),
            display_name: None,
        }));
    }
    let after_action = raw[action.len()..].trim_start();
    let display_name = after_action[name.len()..].trim_start().to_owned();
    Ok(Some(DisplayNameCommand {
        account_id: name.into(),
        display_name: Some(display_name),
    }))
}

pub async fn account_display_name_command(ctx:&maho_ext_api::ExtensionContext,provider:&str,raw:&str)->bool{
    let command=match parse_display_name_command(raw){
        Ok(Some(command))=>command,
        Ok(None)=>return false,
        Err(error)=>{ctx.ui.notify(error,maho_ext_api::NotificationType::Error);return true;}
    };
    match ctx.model_registry.rename_credential_account(provider,&command.account_id,command.display_name.as_deref()).await{
        Ok(())=>{
            let label=command.display_name.as_ref().map_or_else(||command.account_id.clone(),|display|format!("{display} ({})",command.account_id));
            ctx.ui.notify(&format!("Account display name updated: {label}."),maho_ext_api::NotificationType::Info);
        }
        Err(error)=>ctx.ui.notify(&error.message,maho_ext_api::NotificationType::Error),
    }
    true
}

pub async fn prompt_account_display_name(ctx:&maho_ext_api::ExtensionContext,receipt:Option<&maho_ai::auth::types::AccountLoginReceipt>){
    let Some(receipt)=receipt.filter(|receipt|receipt.origin==maho_ai::auth::types::AccountLoginOrigin::Generated)else{return;};
    if ctx.signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted){return;}
    let answer=ctx.ui.input(&format!("Display name for account {} (optional)",receipt.name),Some("Leave blank to keep the account ID"),maho_ext_api::ExtensionUiDialogOptions{signal:ctx.signal.clone(),timeout_ms:None}).await;
    let Some(answer)=answer.filter(|answer|!answer.trim().is_empty())else{return;};
    if ctx.signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted){return;}
    match ctx.model_registry.rename_credential_account(&receipt.provider_id,&receipt.name,Some(&answer)).await{
        Ok(())=>ctx.ui.notify(&format!("Account display name: {answer} ({}).",receipt.name),maho_ext_api::NotificationType::Info),
        Err(error)=>{
            if ctx.signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted)||error.message==crate::oauth_login_interaction::LOGIN_CANCELLED_MESSAGE{return;}
            ctx.ui.notify(&format!("Account is saved, but its display name was not changed: {}",error.message),maho_ext_api::NotificationType::Warning);
        }
    }
}
