use maho_ai::auth::types::{Credential,CredentialStore};
use crate::accounts::{assert_sentinel_invariant,empty_credential,list_accounts};
fn managed(current:Option<&Credential>)->bool {
    current.and_then(Credential::as_oauth).is_some_and(|c|assert_sentinel_invariant(c).is_ok()&&c.extra.get("accounts").is_some_and(serde_json::Value::is_array))
}
pub async fn bootstrap_native(store:&dyn CredentialStore,can_bootstrap:bool)->Option<Credential> {
    let current=store.read(crate::oauth_login::PROVIDER_ID,None).await.ok().flatten();
    if let Some(value)=&current {
        if !managed(Some(value)) {return current;}
        if value.as_oauth().and_then(|c|list_accounts(c).ok()).is_some_and(|slots|!slots.is_empty()) {return current;}
    }
    if !can_bootstrap {return current;}
    let Some(native)=store.read("cursor",None).await.ok().flatten() else {return current;};
    store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |latest|Box::pin(async move {
        if let Some(value)=&latest {
            if !managed(Some(value)) {return Ok(latest);}
            if value.as_oauth().and_then(|c|list_accounts(c).ok()).is_some_and(|slots|!slots.is_empty()) {return Ok(latest);}
        }
        let credential=latest.and_then(Credential::into_oauth).unwrap_or_else(empty_credential);
        Ok(Some(Credential::OAuth(crate::oauth_login::import_native_credential(&credential,Some(&native))?)))
    })),None).await.unwrap_or(current)
}
#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::auth::{credential_store::InMemoryCredentialStore,types::OAuthCredential};
    #[tokio::test]
    async fn gate_and_foreign_credential_preserved() {
        let store=InMemoryCredentialStore::new();assert!(bootstrap_native(&store,false).await.is_none());
        let foreign=Credential::OAuth(OAuthCredential::new("foreign","refresh",1000.0).with_extra("accounts",serde_json::json!([])));
        let expected=foreign.clone();store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |_|Box::pin(async move {Ok(Some(foreign))})),None).await.expect("store");
        assert_eq!(bootstrap_native(&store,true).await,Some(expected));
    }
    #[tokio::test]
    async fn bootstrap_imports_and_later_reads_observe_store() {
        let store=InMemoryCredentialStore::new();let native=Credential::OAuth(OAuthCredential::new("access","refresh",1000.0));
        store.modify("cursor",Box::new(move |_|Box::pin(async move {Ok(Some(native))})),None).await.expect("native");
        let imported=bootstrap_native(&store,true).await.expect("imported");assert_eq!(list_accounts(imported.as_oauth().expect("oauth")).expect("slots")[0].name,"native");
        assert_eq!(bootstrap_native(&store,true).await,Some(imported));
    }
}
