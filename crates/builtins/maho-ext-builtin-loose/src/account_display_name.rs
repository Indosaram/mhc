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
