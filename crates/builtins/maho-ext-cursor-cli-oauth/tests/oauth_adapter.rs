use maho_ai::auth::types::*;
use maho_ext_cursor_cli_oauth::{oauth_login::CursorCliOAuth,settings::CursorCliOauthProviderSettings,accounts::list_accounts};
use std::sync::{Arc,Mutex};
struct Flow;
#[async_trait::async_trait]
impl OAuthAuth for Flow {
    fn name(&self)->&str {"test"}
    async fn login(&self,_:&ProviderAuthInteraction)->anyhow::Result<OAuthCredential> {Ok(OAuthCredential::new("fake-access","fake-refresh",1.0))}
    async fn refresh(&self,_:&OAuthCredential,_:&maho_ai::utils::abort::AbortSignal)->anyhow::Result<OAuthCredential> {Ok(OAuthCredential::new("new-access","new-refresh",1000.0))}
    async fn to_auth(&self,_:&OAuthCredential)->anyhow::Result<ModelAuth> {Ok(ModelAuth::default())}
}
struct Interaction;
#[async_trait::async_trait]
impl AuthInteraction for Interaction {
    fn signal(&self)->Option<maho_ai::utils::abort::AbortSignal> {None}
    async fn prompt(&self,_:AuthPrompt)->anyhow::Result<String> {Ok("yes".into())}
    fn notify(&self,_:AuthEvent) {}
}
#[tokio::test]
async fn login_acknowledges_enables_and_refreshes_slots_with_sentinel() {
    let ack=Arc::new(Mutex::new(Vec::new()));let enabled=Arc::new(Mutex::new(Vec::new()));
    let adapter=CursorCliOAuth {store:Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new()),flow:Arc::new(Flow),settings:Arc::new(CursorCliOauthProviderSettings::default),resolve:Arc::new(|_|Ok(())),persist_acknowledgement:{let ack=ack.clone();Arc::new(move |s|{ack.lock().expect("ack").push(s.to_owned());Ok(())})},persist_enabled:{let enabled=enabled.clone();Arc::new(move |e|{enabled.lock().expect("enabled").push(e);Ok(())})},now:Arc::new(||2)};
    let controller=maho_ai::utils::abort::AbortController::new();let interaction=ProviderAuthInteraction::new(controller.signal(),Arc::new(Interaction));
    let credential=adapter.login(&interaction).await.expect("login");assert_eq!(list_accounts(&credential).expect("slots")[0].name,"default");
    assert_eq!(enabled.lock().expect("enabled").as_slice(),[true]);assert_eq!(ack.lock().expect("ack").len(),1);
    let refreshed=adapter.refresh(&credential,&controller.signal()).await.expect("refresh");let slots=list_accounts(&refreshed).expect("slots");assert_eq!(slots[0].access,"new-access");maho_ext_cursor_cli_oauth::accounts::assert_sentinel_invariant(&refreshed).expect("sentinel");
    assert_eq!(adapter.to_auth(&refreshed).await.expect("request auth").api_key.as_deref(),Some(maho_ext_cursor_cli_oauth::accounts::SENTINEL_TOKEN));
}
