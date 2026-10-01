use crate::config_schema::{Auth,AuthMode,McpServerConfig,Transport};
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum ServerAuthMode {None,Bearer,OAuth}
pub struct ServerAuthPlan {pub mode:ServerAuthMode,pub provider:Option<std::sync::Arc<super::oauth_provider::McpOAuthProvider>>,pub refresh:Option<super::oauth_refresh::McpRefreshManager>}
pub fn resolve_server_auth(server_name:&str,config:&McpServerConfig,agent_dir:&std::path::Path,redirect_url:Option<&str>,client:reqwest::Client)->ServerAuthPlan {
    let mode=resolve_auth_mode(config);
    if mode!=ServerAuthMode::OAuth{return ServerAuthPlan {mode,provider:None,refresh:None};}
    let mut provider=super::oauth_provider::McpOAuthProvider::new(super::token_store::McpTokenStore::new(agent_dir,server_name,config.url.as_deref().unwrap_or("")));
    provider.redirect_url=Some(redirect_url.unwrap_or("http://127.0.0.1:0/callback").into());
    if let Some(oauth)=&config.oauth {provider.scopes=oauth.scopes.clone();provider.client_id=oauth.client_id.clone();provider.client_metadata_url=oauth.client_metadata_url.clone();}
    let provider=std::sync::Arc::new(provider);let refresh=super::oauth_refresh::McpRefreshManager::new(provider.clone(),client);
    ServerAuthPlan {mode,provider:Some(provider),refresh:Some(refresh)}
}
pub fn resolve_auth_mode(config:&McpServerConfig)->ServerAuthMode {
    match &config.auth {
        Some(Auth::Mode(AuthMode::Oauth))=>ServerAuthMode::OAuth,
        Some(Auth::Disabled(false))=>ServerAuthMode::None,
        Some(Auth::Mode(AuthMode::Bearer))=>ServerAuthMode::Bearer,
        Some(Auth::Disabled(true))|None=>{
            if config.bearer_token_env.is_some(){return ServerAuthMode::Bearer;}
            if config.headers.as_ref().is_some_and(|h|!h.is_empty()){return ServerAuthMode::None;}
            if config.transport==Some(Transport::Http) && config.url.as_ref().is_some_and(|u|!u.is_empty()){ServerAuthMode::OAuth}else{ServerAuthMode::None}
        }
    }
}
pub fn detect_literal_bearer_warnings(server:&str,config:&McpServerConfig)->Result<Vec<String>,regex::Error> {
    use sha2::{Digest,Sha256};
    let matcher=regex::Regex::new(r"(?i)(?:bearer\s+\S|sk-[a-z0-9]|[a-z0-9]{24,})")?;
    let mut warnings=Vec::new();
    for (header,value) in config.headers.iter().flat_map(|h|h.iter()) {
        if value.contains("${"){continue;}
        if matcher.is_match(value){let hash=format!("{:x}",Sha256::digest(value));warnings.push(format!("MCP server {server} header '{header}' appears to contain a literal secret (fp {}); use ${{ENV_VAR}} so the token stays out of the config file.",&hash[..8]));}
    }
    Ok(warnings)
}
