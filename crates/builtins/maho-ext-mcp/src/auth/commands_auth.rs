use std::{collections::BTreeMap,path::Path};
use crate::config_schema::{McpServerConfig,Transport,Auth};
use super::{oauth::{OAuthRequestError,begin_authorization,complete_authorization,client_credentials_grant,logout},oauth_provider::McpOAuthProvider,token_store::McpTokenStore,oauth_errors::{OAuthFlowError,OAuthFailureKind}};
pub fn build_provider(name:&str,config:&McpServerConfig,agent_dir:&Path,callback_url:Option<&str>)->Result<McpOAuthProvider,OAuthRequestError> {
    if config.transport!=Some(Transport::Http) || config.auth==Some(Auth::Disabled(false)) || config.url.is_none(){return Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::Headless,format!("MCP server {name} is not an OAuth HTTP server.")))));}
    let mut provider=McpOAuthProvider::new(McpTokenStore::new(agent_dir,name,config.url.as_deref().unwrap_or("")));provider.redirect_url=Some(callback_url.unwrap_or("http://127.0.0.1:0/callback").into());
    if let Some(oauth)=&config.oauth {provider.client_id=oauth.client_id.clone();provider.client_metadata_url=oauth.client_metadata_url.clone();provider.scopes=oauth.scopes.clone();}Ok(provider)
}
pub async fn run_auth_start(name:&str,mut provider:McpOAuthProvider,pending:&mut BTreeMap<String,McpOAuthProvider>,client:&reqwest::Client)->Result<String,OAuthRequestError> {
    let begin=begin_authorization(&mut provider,client).await?;
    let url=begin.authorization_url.ok_or_else(||OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {name} did not produce an authorization URL.")))))?;
    pending.insert(name.into(),provider);Ok(url.to_string())
}
pub async fn run_auth_complete(name:&str,redirect:&str,pending:&mut BTreeMap<String,McpOAuthProvider>,client:&reqwest::Client)->Result<(),OAuthRequestError> {
    let provider=pending.get_mut(name).ok_or_else(||OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::StateMismatch,format!("MCP server {name} authorization state did not match; restart with /mcp auth-start.")))))?;
    complete_authorization(provider,redirect,client).await?;pending.remove(name);Ok(())
}
pub async fn run_client_credentials_auth(provider:&McpOAuthProvider,client:&reqwest::Client)->Result<(),OAuthRequestError> {client_credentials_grant(provider,client).await}
pub async fn run_loopback_auth<F,Fut>(mut provider:McpOAuthProvider,port:Option<u16>,client:&reqwest::Client,on_authorization:F)->Result<(),OAuthRequestError>
where F:FnOnce(url::Url)->Fut,Fut:std::future::Future<Output=Result<(),OAuthRequestError>> {
    let shared=std::sync::Arc::new(std::sync::Mutex::new(None::<McpOAuthProvider>));let state=shared.clone();
    let mut channel=super::callback::open_callback_channel(super::callback::CallbackServerOptions {server_name:provider.store.server_name.clone(),port,host:None,path:None,timeout:None,validate_state:std::sync::Arc::new(move |candidate|state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_mut().is_some_and(|provider|provider.consume_state(candidate)))},None).await.map_err(|error|OAuthRequestError::Flow(Box::new(error)))?;
    provider.redirect_url=Some(channel.redirect_url.clone());
    let begin=begin_authorization(&mut provider,client).await?;
    if begin.authorized {channel.close().await;return Ok(());}
    let authorization=begin.authorization_url.ok_or_else(||OAuthRequestError::Invalid("OAuth flow produced no authorization URL".into()))?;
    *shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(provider);
    on_authorization(authorization).await?;
    let callback=channel.wait_for_code().await.map_err(|error|OAuthRequestError::Flow(Box::new(error)))?;
    let provider=shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().expect("loopback provider initialized before callback");
    super::oauth::finish_authorization(&provider,&callback.code,client).await
}
pub fn run_logout(name:&str,provider:&McpOAuthProvider,pending:&mut BTreeMap<String,McpOAuthProvider>)->Result<(),OAuthRequestError> {logout(provider)?;pending.remove(name);Ok(())}
