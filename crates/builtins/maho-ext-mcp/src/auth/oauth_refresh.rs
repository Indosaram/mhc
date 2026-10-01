use serde_json::Value;
use super::{token_store::McpStoredAuth,oauth_provider::REFRESH_LEEWAY_MS,oauth_errors::{OAuthFlowError,OAuthFailureKind}};
use super::{oauth::{OAuthRequestError,discover,request_tokens},oauth_provider::{McpOAuthProvider,OAuthTokens,merge_tokens_into_stored_auth,stored_auth_to_tokens}};
use std::sync::Arc;
pub struct McpRefreshManager {provider:Arc<McpOAuthProvider>,client:reqwest::Client,gate:tokio::sync::Mutex<()>,pub max_retries:usize,pub retry_delay:std::time::Duration}
impl McpRefreshManager {
    pub fn new(provider:Arc<McpOAuthProvider>,client:reqwest::Client)->Self {Self {provider,client,gate:tokio::sync::Mutex::new(()),max_retries:2,retry_delay:std::time::Duration::from_millis(50)}}
    pub async fn ensure_fresh(&self)->Result<Option<OAuthTokens>,OAuthRequestError> {
        let record=self.provider.store.read()?;let now=chrono::Utc::now().timestamp_millis() as f64;
        if record.as_ref().and_then(|record|record.access_token.as_ref()).is_none(){return Ok(None);}
        if !is_token_stale(record.as_ref(),now){return Ok(stored_auth_to_tokens(record.as_ref(),now));}
        self.refresh().await.map(Some)
    }
    pub async fn refresh(&self)->Result<OAuthTokens,OAuthRequestError> {
        let _guard=self.gate.lock().await;
        let info=discover(&self.provider,&self.client).await?;
        let information=self.provider.client_information()?.ok_or_else(||OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {} is not registered",self.provider.store.server_name)))))?;
        let provider=self.provider.clone();let client=self.client.clone();let max_retries=self.max_retries;let delay=self.retry_delay;let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move ||provider.store.with_lock(|store| {
            let result=runtime.block_on(async {
                let current=store.read()?;let now=chrono::Utc::now().timestamp_millis() as f64;
                if !is_token_stale(current.as_ref(),now) && let Some(tokens)=stored_auth_to_tokens(current.as_ref(),now){return Ok(tokens);}
                let refresh=current.as_ref().and_then(|record|record.refresh_token.as_ref()).ok_or_else(||OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {} has no refresh token",store.server_name)))))?;
                let resource=current.as_ref().and_then(|record|record.resource.as_ref()).unwrap_or(&store.server_url);
                let form=vec![("grant_type".into(),"refresh_token".into()),("refresh_token".into(),refresh.clone()),("resource".into(),resource.clone())];
                for attempt in 0..=max_retries {
                    match request_tokens(&client,&info,&information,form.clone()).await {
                        Ok(tokens)=>{store.write_unlocked(Some(&merge_tokens_into_stored_auth(current.clone(),&tokens,&store.server_url,chrono::Utc::now().timestamp_millis() as f64)))?;return Ok(tokens);}
                        Err(OAuthRequestError::Flow(error)) if error.oauth_kind==OAuthFailureKind::InvalidGrant=>{store.write_unlocked(None)?;return Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::InvalidGrant,format!("MCP server {} refresh rejected (invalid_grant); credentials cleared, re-authentication required.",store.server_name)))));}
                        Err(_) if attempt<max_retries=>{tokio::time::sleep(delay).await;}
                        Err(_)=>return Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::Transient,format!("MCP server {} token refresh failed transiently; will retry on next use.",store.server_name))))),
                    }
                }
                unreachable!("refresh loop always returns on its final attempt")
            });
            Ok(result)
        })).await.map_err(|error|OAuthRequestError::Invalid(error.to_string()))??
    }
}
pub fn is_token_stale(record:Option<&McpStoredAuth>,now:f64)->bool {
    let Some(record)=record else{return true;};
    if record.access_token.as_ref().is_none_or(String::is_empty){return true;}
    record.expires_at.is_some_and(|expires|expires-now<=REFRESH_LEEWAY_MS)
}
pub fn assert_s256_supported(metadata:Option<&Value>,server:&str)->Result<(),Box<OAuthFlowError>> {
    if metadata.and_then(|metadata|metadata.get("code_challenge_methods_supported")).and_then(Value::as_array).is_some_and(|methods|methods.contains(&Value::String("S256".into()))){return Ok(());}
    Err(Box::new(OAuthFlowError::new(OAuthFailureKind::S256Unsupported,format!("MCP server {server} authorization server does not advertise PKCE S256 (code_challenge_methods_supported); refusing to authorize without proof-key protection."))))
}
