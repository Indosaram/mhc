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
pub fn run_logout(name:&str,provider:&McpOAuthProvider,pending:&mut BTreeMap<String,McpOAuthProvider>)->Result<(),OAuthRequestError> {logout(provider)?;pending.remove(name);Ok(())}
