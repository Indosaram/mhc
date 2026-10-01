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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_route_actions() {
        assert_eq!(parse_action(""),AccountAction::List);assert_eq!(parse_action(" pin work "),AccountAction::Pin("work".into()));assert_eq!(parse_action("import native"),AccountAction::ImportNative);assert_eq!(parse_action("import invalid"),AccountAction::Usage);assert_eq!(parse_action("remove"),AccountAction::Usage);
    }
}
