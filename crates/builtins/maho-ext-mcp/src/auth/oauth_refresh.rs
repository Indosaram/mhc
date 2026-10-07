use serde_json::Value;
use super::{token_store::McpStoredAuth,oauth_provider::REFRESH_LEEWAY_MS,oauth_errors::{OAuthFlowError,OAuthFailureKind}};
use super::{oauth::{OAuthRequestError,OAuthServerInfo,request_tokens},oauth_provider::{McpOAuthProvider,OAuthTokens,merge_tokens_into_stored_auth,stored_auth_to_tokens}};
use futures::FutureExt;
use std::sync::Arc;
type RefreshFuture=futures::future::Shared<futures::future::BoxFuture<'static,Result<OAuthTokens,OAuthRequestError>>>;
/// Injection seam for the refresh manager (upstream `RefreshManagerOptions`,
/// `mcp/auth/oauth-refresh.ts:15`). The pinned constructor takes `fetchFn`, `maxRetries`,
/// `retryDelayMs` and `discover`; this port keeps the transport client as the `fetchFn`
/// equivalent (it is passed in), and this struct carries the rest.
pub type DiscoverFn=Arc<dyn Fn(&str)->futures::future::BoxFuture<'static,Result<OAuthServerInfo,OAuthRequestError>>+Send+Sync>;
#[derive(Clone,Default)]
pub struct RefreshManagerOptions { pub discover:Option<DiscoverFn>, pub max_retries:Option<usize>, pub retry_delay_ms:Option<u64> }
pub struct McpRefreshManager {provider:Arc<McpOAuthProvider>,client:reqwest::Client,inflight:tokio::sync::Mutex<Option<Arc<RefreshFuture>>>,discover:Option<DiscoverFn>,pub max_retries:usize,pub retry_delay:std::time::Duration}
impl McpRefreshManager {
    pub fn new(provider:Arc<McpOAuthProvider>,client:reqwest::Client)->Self {Self::new_with_options(provider,client,RefreshManagerOptions::default())}
    pub fn new_with_options(provider:Arc<McpOAuthProvider>,client:reqwest::Client,options:RefreshManagerOptions)->Self {Self {provider,client,inflight:tokio::sync::Mutex::new(None),discover:options.discover,max_retries:options.max_retries.unwrap_or(2),retry_delay:std::time::Duration::from_millis(options.retry_delay_ms.unwrap_or(50))}}
    pub async fn ensure_fresh(&self)->Result<Option<OAuthTokens>,OAuthRequestError> {
        let record=self.provider.store.read()?;let now=chrono::Utc::now().timestamp_millis() as f64;
        if record.as_ref().and_then(|record|record.access_token.as_ref()).is_none(){return Ok(None);}
        if !is_token_stale(record.as_ref(),now){return Ok(stored_auth_to_tokens(record.as_ref(),now));}
        self.refresh().await.map(Some)
    }
    pub async fn refresh(&self)->Result<OAuthTokens,OAuthRequestError> {self.refresh_inner(false).await}
    /// Pinned `oauth-handler.ts::handlePostRequestAuthError`: a rejected request refreshes even
    /// when the stored token has not reached the leeway window, so this skips the staleness
    /// short-circuit that `refresh` keeps.
    pub async fn force_refresh(&self)->Result<OAuthTokens,OAuthRequestError> {self.refresh_inner(true).await}
    async fn refresh_inner(&self,force:bool)->Result<OAuthTokens,OAuthRequestError> {
        // oauth-refresh.ts coalesces every in-process caller onto one in-flight
        // promise and clears it once settled, so a later call retries.
        let shared={let mut slot=self.inflight.lock().await;match slot.as_ref() {Some(existing)=>existing.clone(),None=>{let shared:Arc<RefreshFuture>=Arc::new(self.refresh_locked(force).shared());*slot=Some(shared.clone());shared}}};
        let result=shared.as_ref().clone().await;
        let mut slot=self.inflight.lock().await;
        if slot.as_ref().is_some_and(|current|Arc::ptr_eq(current,&shared)) {*slot=None;}
        result
    }
    /// Pinned `oauth-handler.ts::handleStepUpIfNeeded` -> `mergeScopes`: the challenge scopes are
    /// unioned into the provider's requested scope set.
    pub fn escalate_scopes(&self,required:&[String])->Vec<String> {self.provider.escalate_scopes(required)}
    fn refresh_locked(&self,force:bool)->futures::future::BoxFuture<'static,Result<OAuthTokens,OAuthRequestError>> {
        let provider=self.provider.clone();let client=self.client.clone();let max_retries=self.max_retries;let delay=self.retry_delay;let discover=self.discover.clone();
        async move {
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move ||provider.store.with_lock(|store| {
            let result=runtime.block_on(async {
                let current=store.read()?;let now=chrono::Utc::now().timestamp_millis() as f64;
                if !force && !is_token_stale(current.as_ref(),now) && let Some(tokens)=stored_auth_to_tokens(current.as_ref(),now){return Ok(tokens);}
                let refresh=current.as_ref().and_then(|record|record.refresh_token.clone()).ok_or_else(||OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {} has no refresh token",store.server_name)))))?;
                let info=match &discover {Some(discover)=>discover(&store.server_url).await?,None=>super::oauth::discover(&provider,&client).await?};
                let information=provider.client_information()?.ok_or_else(||OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {} is not registered",store.server_name)))))?;
                let resource=current.as_ref().and_then(|record|record.resource.clone()).unwrap_or_else(||provider.resource_indicator());
                let form=vec![("grant_type".into(),"refresh_token".into()),("refresh_token".into(),refresh.clone()),("resource".into(),resource.clone())];
                for attempt in 0..=max_retries {
                    match request_tokens(&client,&info,&information,form.clone()).await {
                        Ok(tokens)=>{store.write_unlocked(Some(&merge_tokens_into_stored_auth(current.clone(),&tokens,&store.server_url,chrono::Utc::now().timestamp_millis() as f64)))?;return Ok(tokens);}
                        Err(OAuthRequestError::Flow(error)) if error.oauth_kind==OAuthFailureKind::InvalidGrant=>{store.write_unlocked(None)?;return Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::InvalidGrant,format!("MCP server {} refresh rejected (invalid_grant); credentials cleared, re-authentication required.",store.server_name)))));}
                        Err(error) if attempt<max_retries && match &error {
                            OAuthRequestError::Flow(flow)=>super::oauth_errors::is_transient_token_error(&serde_json::json!({"errorCode":flow.error.message,"message":flow.error.message})),
                            OAuthRequestError::Http(http) if http.is_connect()=>true,
                            _=>super::oauth_errors::is_transient_token_error(&Value::String(error.to_string())),
                        }=>{tokio::time::sleep(delay).await;}
                        Err(_)=>return Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::Transient,format!("MCP server {} token refresh failed transiently; will retry on next use.",store.server_name))))),
                    }
                }
                unreachable!("refresh loop always returns on its final attempt")
            });
            Ok(result)
        })).await.map_err(|error|OAuthRequestError::Invalid(error.to_string()))??
        }.boxed()
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
