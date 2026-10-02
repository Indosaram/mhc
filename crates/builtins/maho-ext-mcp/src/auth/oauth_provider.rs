use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use super::{oauth_errors::{OAuthFailureKind, OAuthFlowError},token_store::{McpStoredAuth,McpTokenStore,TokenStoreError}};
pub const REFRESH_LEEWAY_MS: f64 = 300000.0;
pub type McpRedirectHandler=std::sync::Arc<dyn Fn(url::Url)->std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),ProviderError>>+Send>>+Send+Sync>;
#[derive(Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub refresh_token: Option<String>,
    pub token_type: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub expires_in: Option<f64>,
}
pub fn token_expires_at(tokens: &OAuthTokens, now: f64) -> Option<f64> { tokens.expires_in.map(|s| now+s*1000.0) }
pub fn stored_auth_to_tokens(record: Option<&McpStoredAuth>, now: f64) -> Option<OAuthTokens> {
    let record = record?;
    let access = record.access_token.as_ref().filter(|s| !s.is_empty())?;
    Some(OAuthTokens { access_token:access.clone(),refresh_token:record.refresh_token.clone(),token_type:"Bearer".into(),expires_in:record.expires_at.map(|t| ((t-now)/1000.0).ceil().max(0.0)) })
}
pub fn merge_tokens_into_stored_auth(current: Option<McpStoredAuth>, tokens: &OAuthTokens, server_url: &str, now: f64) -> McpStoredAuth {
    let mut next = current.unwrap_or_default(); next.access_token = Some(tokens.access_token.clone());
    next.resource.get_or_insert_with(|| server_url.into());
    if let Some(refresh) = &tokens.refresh_token { next.refresh_token = Some(refresh.clone()); }
    next.expires_at = token_expires_at(tokens,now); next
}
pub struct McpOAuthProvider {
    pub store: McpTokenStore, pub redirect_url: Option<String>, pub scopes: Option<Vec<String>>,
    pub client_id: Option<String>, pub client_metadata_url: Option<String>,
    pub logger:Option<std::sync::Arc<std::sync::Mutex<crate::log::McpLogger>>>,
    pub on_redirect:Option<McpRedirectHandler>,
    expected_state: Option<String>, pub last_authorization_url: Option<url::Url>,
}
#[derive(Clone, Copy)]
pub enum CredentialScope { All, Client, Tokens, Verifier, Discovery }
impl McpOAuthProvider {
    pub fn new(store: McpTokenStore) -> Self { Self { store,redirect_url:None,scopes:None,client_id:None,client_metadata_url:None,logger:None,on_redirect:None,expected_state:None,last_authorization_url:None } }
    pub fn client_metadata(&self) -> Value {
        let mut value = json!({"redirect_uris":self.redirect_url.iter().collect::<Vec<_>>(),"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":"none","client_name":"senpi"});
        if let Some(scopes) = &self.scopes { value["scope"] = Value::String(scopes.join(" ")); }
        value
    }
    pub fn state(&mut self) -> Result<String, getrandom::Error> {
        let mut bytes = [0u8;16]; getrandom::fill(&mut bytes)?;
        let state = URL_SAFE_NO_PAD.encode(bytes); self.expected_state = Some(state.clone()); Ok(state)
    }
    pub fn consume_state(&mut self, candidate: Option<&str>) -> bool { let expected = self.expected_state.take(); expected.as_deref().is_some_and(|s| Some(s) == candidate) }
    pub fn client_information(&self) -> Result<Option<Value>,TokenStoreError> {
        if let Some(id) = self.client_id.as_ref().or(self.client_metadata_url.as_ref()) { return Ok(Some(json!({"client_id":id}))); }
        Ok(self.store.read()?.and_then(|r| r.client_info))
    }
    pub fn save_client_information(&self, info: Value) -> Result<(),TokenStoreError> { self.store.update(|r| { let mut next = r.unwrap_or_default(); next.client_info=Some(info); Some(next) })?; Ok(()) }
    pub fn save_discovery_state(&self, state: Value) -> Result<(),TokenStoreError> { self.store.update(|r| { let mut next = r.unwrap_or_default(); next.discovery_state=Some(state); Some(next) })?; Ok(()) }
    pub fn discovery_state(&self) -> Result<Option<Value>,TokenStoreError> { Ok(self.store.read()?.and_then(|r| r.discovery_state)) }
    pub fn tokens(&self, now: f64) -> Result<Option<OAuthTokens>,TokenStoreError> { Ok(stored_auth_to_tokens(self.store.read()?.as_ref(),now)) }
    pub fn save_tokens(&self, tokens: &OAuthTokens, now: f64) -> Result<(),TokenStoreError> {
        self.store.update(|r| Some(merge_tokens_into_stored_auth(r,tokens,&self.store.server_url,now)))?;
        if let Some(logger)=&self.logger {let _=logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log("info","oauth saveTokens",Some(&json!({"token_fp":crate::log::fingerprint_secret(&tokens.access_token)})),None);}
        Ok(())
    }
    pub fn save_code_verifier(&self, verifier: &str) -> Result<(),TokenStoreError> { self.store.update(|r| { let mut next = r.unwrap_or_default(); next.code_verifier=Some(verifier.into()); Some(next) })?; Ok(()) }
    pub fn code_verifier(&self) -> Result<String, ProviderError> {
        self.store.read()?.and_then(|r| r.code_verifier).filter(|s| !s.is_empty()).ok_or_else(|| ProviderError::OAuth(Box::new(OAuthFlowError::new(OAuthFailureKind::NoVerifier,format!("Missing PKCE code verifier for MCP server {}",self.store.server_name)))))
    }
    pub async fn redirect_to_authorization(&mut self, authorization_url: url::Url)->Result<(),ProviderError> {
        self.last_authorization_url=Some(authorization_url.clone());
        if let Some(handler)=&self.on_redirect {handler(authorization_url).await?;}
        Ok(())
    }
    pub fn validate_resource_url(server_url: &str, resource: Option<&str>) -> Result<url::Url,url::ParseError> { url::Url::parse(resource.unwrap_or(server_url)) }
    pub fn invalidate_credentials(&self, scope: CredentialScope) -> Result<(),TokenStoreError> {
        if matches!(scope,CredentialScope::All) { return self.store.clear(); }
        self.store.update(|current| current.map(|mut next| {
            match scope { CredentialScope::All => (), CredentialScope::Tokens => { next.access_token=None; next.refresh_token=None; next.expires_at=None; }, CredentialScope::Client => next.client_info=None, CredentialScope::Verifier => next.code_verifier=None, CredentialScope::Discovery => next.discovery_state=None }
            next
        }))?; Ok(())
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ProviderError { #[error(transparent)] Store(#[from] TokenStoreError), #[error(transparent)] OAuth(Box<OAuthFlowError>) }
