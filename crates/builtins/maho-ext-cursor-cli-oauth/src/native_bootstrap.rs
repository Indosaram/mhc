use maho_ai::auth::types::{Credential,CredentialStore};
use crate::accounts::{assert_sentinel_invariant,empty_credential,list_accounts};
fn managed(current:Option<&Credential>)->bool {
    current.and_then(Credential::as_oauth).is_some_and(|c|assert_sentinel_invariant(c).is_ok()&&c.extra.get("accounts").is_some_and(serde_json::Value::is_array))
}
type BootstrapResult=Option<Option<Credential>>;
pub struct CredentialReader {
    store:std::sync::Arc<dyn CredentialStore>,
    can_bootstrap:std::sync::Arc<dyn Fn()->bool+Send+Sync>,
    in_flight:std::sync::Arc<std::sync::Mutex<Option<tokio::sync::watch::Receiver<BootstrapResult>>>>,
}
impl CredentialReader {
    pub fn new(store:std::sync::Arc<dyn CredentialStore>,can_bootstrap:std::sync::Arc<dyn Fn()->bool+Send+Sync>)->Self {Self {store,can_bootstrap,in_flight:Default::default()}}
    pub async fn read(&self)->Option<Credential> {
        let mut receiver={
            let mut flight=self.in_flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(receiver)=flight.as_ref() {receiver.clone()}else {
                let (sender,receiver)=tokio::sync::watch::channel(None);*flight=Some(receiver.clone());
                let store=self.store.clone();let gate=self.can_bootstrap.clone();let flight=self.in_flight.clone();
                tokio::spawn(async move {let result=bootstrap_native(store.as_ref(),gate()).await;let mut flight=flight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);sender.send_replace(Some(result));*flight=None;});receiver
            }
        };
        loop {if let Some(result)=receiver.borrow_and_update().clone() {return result;}
            if receiver.changed().await.is_err() {return None;}}
    }
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
    #[tokio::test]
    async fn concurrent_reads_share_flight_and_later_reads_reload() {
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
        let store=Arc::new(InMemoryCredentialStore::new());let calls=Arc::new(AtomicUsize::new(0));let reader=CredentialReader::new(store.clone(),{let calls=calls.clone();Arc::new(move ||{calls.fetch_add(1,Ordering::SeqCst);false})});
        let (first,second)=tokio::join!(reader.read(),reader.read());assert!(first.is_none()&&second.is_none());assert_eq!(calls.load(Ordering::SeqCst),1);
        let foreign=Credential::OAuth(OAuthCredential::new("test","test",1000.0));let expected=foreign.clone();store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |_|Box::pin(async move {Ok(Some(foreign))})),None).await.expect("store");assert_eq!(reader.read().await,Some(expected));
    }
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
