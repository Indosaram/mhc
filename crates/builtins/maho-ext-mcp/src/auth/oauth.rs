use super::oauth_errors::{OAuthFailureKind,OAuthFlowError};
use super::{oauth_provider::{McpOAuthProvider,OAuthTokens,ProviderError,CredentialScope},oauth_refresh::assert_s256_supported};
use serde_json::Value;
use sha2::{Digest,Sha256};
use base64::{Engine,engine::general_purpose::URL_SAFE_NO_PAD};
// Arc-wrapped payloads keep the error cloneable for the cross-caller refresh
// single-flight (oauth-refresh.ts shares one in-flight promise with every caller).
#[derive(Debug,Clone,thiserror::Error)]
pub enum OAuthRequestError {
    #[error(transparent)] Http(std::sync::Arc<reqwest::Error>),
    #[error(transparent)] Store(std::sync::Arc<super::token_store::TokenStoreError>),
    #[error(transparent)] Provider(std::sync::Arc<ProviderError>),
    #[error(transparent)] Flow(Box<OAuthFlowError>),
    #[error("{0}")] Invalid(String),
    #[error(transparent)] Url(#[from] url::ParseError),
    #[error(transparent)] Json(std::sync::Arc<serde_json::Error>),
}
impl From<reqwest::Error> for OAuthRequestError {fn from(error:reqwest::Error)->Self {Self::Http(std::sync::Arc::new(error))}}
impl From<super::token_store::TokenStoreError> for OAuthRequestError {fn from(error:super::token_store::TokenStoreError)->Self {Self::Store(std::sync::Arc::new(error))}}
impl From<ProviderError> for OAuthRequestError {fn from(error:ProviderError)->Self {Self::Provider(std::sync::Arc::new(error))}}
impl From<serde_json::Error> for OAuthRequestError {fn from(error:serde_json::Error)->Self {Self::Json(std::sync::Arc::new(error))}}
#[derive(Clone,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub struct OAuthServerInfo {pub authorization_server_url:String,pub authorization_server_metadata:Value,pub resource_metadata:Value}
pub async fn discover(provider:&McpOAuthProvider,client:&reqwest::Client)->Result<OAuthServerInfo,OAuthRequestError> {
    if let Some(cached)=provider.discovery_state()? {return Ok(serde_json::from_value(cached)?);}
    let info=discover_uncached(provider,client).await?;
    provider.save_discovery_state(serde_json::to_value(&info)?)?;Ok(info)
}
pub(crate) async fn discover_uncached(provider:&McpOAuthProvider,client:&reqwest::Client)->Result<OAuthServerInfo,OAuthRequestError> {
    let resource=url::Url::parse(&provider.store.server_url)?;
    let mut metadata_url=resource.clone();metadata_url.set_path(&format!("/.well-known/oauth-protected-resource{}",resource.path()));metadata_url.set_query(None);
    let mut metadata=Value::Null;
    for path in [metadata_url.path().to_owned(),"/.well-known/oauth-protected-resource".into()] {
        metadata_url.set_path(&path);
        if let Ok(response)=client.get(metadata_url.clone()).header("accept","application/json").header("mcp-protocol-version","2025-11-25").send().await && response.status().is_success() && let Ok(value)=response.json::<Value>().await {metadata=value;break;}
    }
    let fallback=resource.join("/")?.to_string();
    let issuer=metadata.get("authorization_servers").and_then(Value::as_array).and_then(|servers|servers.first()).and_then(Value::as_str).unwrap_or(&fallback);
    let mut discovery=url::Url::parse(issuer)?;let path=discovery.path().trim_end_matches('/').to_owned();discovery.set_query(None);discovery.set_fragment(None);
    let mut paths=vec![format!("/.well-known/oauth-authorization-server{path}"),format!("/.well-known/openid-configuration{path}")];
    if !path.is_empty(){paths.push(format!("{path}/.well-known/openid-configuration"));}
    let mut authorization=Value::Null;
    for path in paths {
        discovery.set_path(&path);
        let response=client.get(discovery.clone()).header("accept","application/json").header("mcp-protocol-version","2025-11-25").send().await?;
        if response.status().is_client_error(){continue;}
        authorization=response.error_for_status()?.json::<Value>().await?;break;
    }
    Ok(OAuthServerInfo {authorization_server_url:issuer.into(),authorization_server_metadata:authorization,resource_metadata:metadata})
}
pub struct BeginAuthResult {pub authorized:bool,pub authorization_url:Option<url::Url>}
pub async fn begin_authorization(provider:&mut McpOAuthProvider,client:&reqwest::Client)->Result<BeginAuthResult,OAuthRequestError> {
    let info=discover(provider,client).await?;
    assert_s256_supported(Some(&info.authorization_server_metadata),&provider.store.server_name).map_err(OAuthRequestError::Flow)?;
    if provider.tokens(chrono::Utc::now().timestamp_millis() as f64)?.is_some(){return Ok(BeginAuthResult {authorized:true,authorization_url:None});}
    let client_info=match provider.client_information()? {
        Some(info)=>info,
        None=>{
            let endpoint=info.authorization_server_metadata.get("registration_endpoint").and_then(Value::as_str).ok_or_else(||OAuthRequestError::Invalid("OAuth authorization server has no registration endpoint".into()))?;
            let registered=client.post(endpoint).json(&provider.client_metadata()).send().await?.error_for_status()?.json::<Value>().await?;
            provider.save_client_information(registered.clone())?;registered
        }
    };
    let client_id=client_info.get("client_id").and_then(Value::as_str).ok_or_else(||OAuthRequestError::Invalid("OAuth client has no client_id".into()))?;
    let mut bytes=[0u8;32];getrandom::fill(&mut bytes).map_err(|error|OAuthRequestError::Invalid(error.to_string()))?;
    let verifier=URL_SAFE_NO_PAD.encode(bytes);provider.save_code_verifier(&verifier)?;
    let challenge=URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state=provider.state().map_err(|error|OAuthRequestError::Invalid(error.to_string()))?;
    let endpoint=info.authorization_server_metadata.get("authorization_endpoint").and_then(Value::as_str).ok_or_else(||OAuthRequestError::Invalid("OAuth metadata has no authorization endpoint".into()))?;
    let mut authorization=url::Url::parse(endpoint)?;
    {let mut query=authorization.query_pairs_mut();query.append_pair("response_type","code").append_pair("client_id",client_id).append_pair("code_challenge_method","S256").append_pair("code_challenge",&challenge).append_pair("state",&state).append_pair("resource",&provider.store.server_url);
        if let Some(redirect)=&provider.redirect_url {query.append_pair("redirect_uri",redirect);}
        if let Some(scopes)=&provider.scopes {query.append_pair("scope",&scopes.join(" "));}}
    provider.redirect_to_authorization(authorization.clone()).await?;Ok(BeginAuthResult {authorized:false,authorization_url:Some(authorization)})
}
pub async fn complete_authorization(provider:&mut McpOAuthProvider,redirect:&str,client:&reqwest::Client)->Result<(),OAuthRequestError> {
    let (code,state)=parse_redirect(redirect,&provider.store.server_name).map_err(OAuthRequestError::Flow)?;
    if !provider.consume_state(state.as_deref()){return Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::StateMismatch,format!("MCP server {} authorization state did not match (possible CSRF or a stale/replayed link); restart with /mcp auth-start.",provider.store.server_name)))));}
    finish_authorization(provider,&code,client).await
}
pub async fn finish_authorization(provider:&McpOAuthProvider,code:&str,client:&reqwest::Client)->Result<(),OAuthRequestError> {
    let info=discover(provider,client).await?;
    let client_info=provider.client_information()?.ok_or_else(||OAuthRequestError::Invalid("OAuth client is not registered".into()))?;
    let mut form=vec![("grant_type".into(),"authorization_code".into()),("code".into(),code.into()),("code_verifier".into(),provider.code_verifier()?), ("resource".into(),provider.store.server_url.clone())];
    if let Some(redirect)=&provider.redirect_url {form.push(("redirect_uri".into(),redirect.clone()));}
    match request_tokens(client,&info,&client_info,form).await {
        Ok(tokens)=>{provider.save_tokens(&tokens,chrono::Utc::now().timestamp_millis() as f64)?;Ok(())}
        Err(OAuthRequestError::Flow(error)) if error.oauth_kind==OAuthFailureKind::InvalidGrant=>{
            provider.invalidate_credentials(CredentialScope::Tokens)?;provider.invalidate_credentials(CredentialScope::Verifier)?;
            Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::ExpiredCode,format!("MCP server {} authorization code was rejected or expired; restart with /mcp auth-start {}.",provider.store.server_name,provider.store.server_name)))))
        }
        Err(error)=>Err(error),
    }
}
pub async fn request_tokens(client:&reqwest::Client,info:&OAuthServerInfo,client_info:&Value,mut form:Vec<(String,String)>)->Result<OAuthTokens,OAuthRequestError> {
    let endpoint=info.authorization_server_metadata.get("token_endpoint").and_then(Value::as_str).ok_or_else(||OAuthRequestError::Invalid("OAuth metadata has no token endpoint".into()))?;
    let id=client_info.get("client_id").and_then(Value::as_str).ok_or_else(||OAuthRequestError::Invalid("OAuth client has no client_id".into()))?;
    let secret=client_info.get("client_secret").and_then(Value::as_str);
    let methods=info.authorization_server_metadata.get("token_endpoint_auth_methods_supported").and_then(Value::as_array).map_or(&[][..],Vec::as_slice);
    let supported=|method:&str|methods.iter().any(|value|value.as_str()==Some(method));
    let preferred=client_info.get("token_endpoint_auth_method").and_then(Value::as_str).filter(|method|["client_secret_basic","client_secret_post","none"].contains(method) && (methods.is_empty() || supported(method)));
    let method=preferred.unwrap_or_else(|| {
        if methods.is_empty(){if secret.is_some(){"client_secret_basic"}else{"none"}}
        else if secret.is_some() && supported("client_secret_basic"){"client_secret_basic"}
        else if secret.is_some() && supported("client_secret_post"){"client_secret_post"}
        else if supported("none") || secret.is_none(){"none"}else{"client_secret_post"}
    });
    let mut request=client.post(endpoint).header("content-type","application/x-www-form-urlencoded");
    match method {
        "client_secret_basic"=>{
            let secret=secret.filter(|secret|!secret.is_empty()).ok_or_else(||OAuthRequestError::Invalid("client_secret_basic authentication requires a client_secret".into()))?;
            request=request.basic_auth(id,Some(secret));
        }
        "client_secret_post"=>{
            form.push(("client_id".into(),id.into()));
            if let Some(secret)=secret.filter(|secret|!secret.is_empty()){form.push(("client_secret".into(),secret.into()));}
        }
        _=>form.push(("client_id".into(),id.into())),
    }
    let body=url::form_urlencoded::Serializer::new(String::new()).extend_pairs(&form).finish();
    let response=request.body(body).send().await?;
    let status=response.status();let body=response.json::<Value>().await?;
    if !status.is_success() {
        let code=body.get("error").and_then(Value::as_str).unwrap_or("token_error");
        let kind=if ["invalid_grant","invalid_token","invalid_client","unauthorized_client"].contains(&code){OAuthFailureKind::InvalidGrant}else{OAuthFailureKind::Transient};
        return Err(OAuthRequestError::Flow(Box::new(OAuthFlowError::new(kind,code))));
    }
    Ok(serde_json::from_value(body)?)
}
pub async fn client_credentials_grant(provider:&McpOAuthProvider,client:&reqwest::Client)->Result<(),OAuthRequestError> {
    let info=discover(provider,client).await?;let information=provider.client_information()?.ok_or_else(||OAuthRequestError::Flow(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {} client_credentials requires a clientId.",provider.store.server_name)))))?;
    let mut form=vec![("grant_type".into(),"client_credentials".into()),("resource".into(),provider.store.server_url.clone())];
    if let Some(scopes)=&provider.scopes {form.push(("scope".into(),scopes.join(" ")));}
    let tokens=request_tokens(client,&info,&information,form).await?;provider.save_tokens(&tokens,chrono::Utc::now().timestamp_millis() as f64)?;Ok(())
}
pub fn logout(provider:&McpOAuthProvider)->Result<(),OAuthRequestError> {provider.store.clear()?;Ok(())}
pub fn parse_redirect(input:&str,server:&str)->Result<(String,Option<String>),Box<OAuthFlowError>> {
    let url=url::Url::parse(input.trim()).map_err(|_|Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {server} received a malformed redirect URL; paste the full http://127.0.0.1/... address from your browser."))))?;
    let params=url.query_pairs().collect::<Vec<_>>();
    if let Some((_,error))=params.iter().find(|(key,_)|key=="error") {return Err(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {server} authorization failed: {error}"))));}
    let code=params.iter().find(|(key,_)|key=="code").map(|(_,value)|value.to_string()).filter(|value|!value.is_empty()).ok_or_else(||Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {server} redirect URL has no authorization code; ensure you copied the entire address."))))?;
    Ok((code,params.iter().find(|(key,_)|key=="state").map(|(_,value)|value.to_string())))
}
pub fn is_rejected_authorization_code(message:&str)->bool {
    let message=message.to_lowercase();message.contains("authorization code invalid") || (message.contains("authorization code") && message.contains("used")) || message.contains("pkce verification failed")
}
