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
#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub struct OAuthServerInfo {pub authorization_server_url:String,pub authorization_server_metadata:Value,pub resource_metadata:Value}
type InfoFuture=futures::future::Shared<futures::future::BoxFuture<'static,Result<OAuthServerInfo,OAuthRequestError>>>;
static DISCOVERY_CACHE:std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String,OAuthServerInfo>>>=std::sync::LazyLock::new(Default::default);
static DISCOVERY_INFLIGHT:std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String,(u64,InfoFuture)>>>=std::sync::LazyLock::new(Default::default);
static NEXT_FLIGHT_ID:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(1);
fn locked<T>(mutex:&std::sync::Mutex<T>)->std::sync::MutexGuard<'_,T> {mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)}
/// RFC 8707 resource value: query and fragment dropped, one trailing `/` removed.
pub fn normalize_resource_url(server_url:&str)->String {
    let Ok(mut url)=url::Url::parse(server_url) else{return server_url.to_owned();};
    url.set_query(None);url.set_fragment(None);
    let mut text=url.to_string();
    if text.ends_with('/'){text.pop();}
    text
}
/// Sets `resource` on a form: replaces the first occurrence in place, drops later duplicates, else appends.
pub fn add_resource_param(form:&mut Vec<(String,String)>,resource:&str) {
    let mut seen=false;
    form.retain_mut(|(key,value)|{
        if key!="resource"{return true;}
        if seen{return false;}
        seen=true;*value=resource.to_owned();true
    });
    if !seen{form.push(("resource".into(),resource.into()));}
}
/// Clears the process-wide discovery cache and the in-flight table.
pub fn reset_discovery_cache() {locked(&DISCOVERY_CACHE).clear();locked(&DISCOVERY_INFLIGHT).clear();}
fn check_https(url:&str,label:&str)->Result<(),OAuthRequestError> {
    match url::Url::parse(url) {Ok(parsed) if parsed.scheme()=="https"=>Ok(()),_=>Err(OAuthRequestError::Invalid(format!("{label} must use https")))}
}
/// Enforces the https rule on the server URL, issuer and every endpoint of discovered metadata.
pub fn validate_info_https(server_url:&str,info:&OAuthServerInfo)->Result<(),OAuthRequestError> {
    check_https(server_url,"Resource server URL")?;
    check_https(&info.authorization_server_url,"Authorization server URL")?;
    for field in ["authorization_endpoint","token_endpoint","registration_endpoint"] {
        if let Some(endpoint)=info.authorization_server_metadata.get(field).and_then(Value::as_str) {check_https(endpoint,field)?;}
    }
    Ok(())
}
pub async fn discover(provider:&McpOAuthProvider,client:&reqwest::Client)->Result<OAuthServerInfo,OAuthRequestError> {
    let server_url=provider.store.server_url.clone();
    let key=normalize_resource_url(&server_url);
    let cached=locked(&DISCOVERY_CACHE).get(&key).cloned();
    if let Some(info)=cached {
        if provider.require_https{validate_info_https(&server_url,&info)?;}
        return Ok(info);
    }
    let shared={
        let mut inflight=locked(&DISCOVERY_INFLIGHT);
        match inflight.get(&key) {
            Some((_,existing))=>existing.clone(),
            None=>{
                let id=NEXT_FLIGHT_ID.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
                let (fetch_url,require,http,flight_key)=(server_url.clone(),provider.require_https,client.clone(),key.clone());
                let shared:InfoFuture=futures::FutureExt::shared(futures::FutureExt::boxed(async move {
                    let result=discover_fetch(fetch_url,require,http).await;
                    if let Ok(info)=&result {locked(&DISCOVERY_CACHE).insert(flight_key.clone(),info.clone());}
                    let mut inflight=locked(&DISCOVERY_INFLIGHT);
                    if inflight.get(&flight_key).is_some_and(|(current,_)|*current==id){inflight.remove(&flight_key);}
                    result
                }));
                inflight.insert(key,(id,shared.clone()));shared
            }
        }
    };
    let info=shared.await?;
    if provider.require_https{validate_info_https(&server_url,&info)?;}
    Ok(info)
}
async fn get_metadata(client:&reqwest::Client,url:url::Url)->Result<reqwest::Response,OAuthRequestError> {
    Ok(client.get(url).header("accept","application/json").header("mcp-protocol-version","2025-11-25").send().await?)
}
async fn read_metadata(response:reqwest::Response,what:&str)->Result<Value,OAuthRequestError> {
    let text=response.text().await?;
    serde_json::from_str::<Value>(&text).ok().filter(|value|!value.is_null()).ok_or_else(||OAuthRequestError::Invalid(format!("{what} is not valid JSON")))
}
async fn discover_fetch(server_url:String,require_https:bool,client:reqwest::Client)->Result<OAuthServerInfo,OAuthRequestError> {
    let resource=url::Url::parse(&server_url)?;
    if require_https{check_https(&server_url,"Resource server URL")?;}
    let mut metadata_url=resource.clone();metadata_url.set_path("/.well-known/oauth-protected-resource");metadata_url.set_query(None);metadata_url.set_fragment(None);
    let response=get_metadata(&client,metadata_url).await?;
    let status=response.status();
    let (issuer,resource_metadata)=if status.is_success() {
        let value=read_metadata(response,"Protected resource metadata").await?;
        let issuer=value.get("authorization_servers").and_then(Value::as_array).and_then(|servers|servers.iter().filter_map(Value::as_str).find(|server|!server.is_empty())).map(str::to_owned)
            .ok_or_else(||OAuthRequestError::Invalid("Protected resource metadata lacks authorization_servers".into()))?;
        (issuer,value)
    } else if status.as_u16()==404 {(server_url.clone(),Value::Null)}
    else {return Err(OAuthRequestError::Invalid(format!("Protected resource metadata request failed with status {}",status.as_u16())));};
    let issuer_url=url::Url::parse(&issuer)?;
    if require_https{check_https(&issuer,"Authorization server URL")?;}
    let mut base=issuer_url.clone();base.set_query(None);base.set_fragment(None);
    let path=issuer_url.path().trim_end_matches('/').to_owned();
    let mut paths=vec![format!("/.well-known/oauth-authorization-server{path}")];
    if !path.is_empty(){paths.push("/.well-known/oauth-authorization-server".into());}
    let mut found=None;
    for (index,candidate) in paths.iter().enumerate() {
        let mut url=base.clone();url.set_path(candidate);
        let response=get_metadata(&client,url).await?;
        let status=response.status();
        if status.is_success(){found=Some(read_metadata(response,"Authorization server metadata").await?);break;}
        if status.as_u16()==404 {
            if index+1<paths.len(){continue;}
            return Err(OAuthRequestError::Invalid("Authorization server metadata not found".into()));
        }
        return Err(OAuthRequestError::Invalid(format!("Authorization server metadata request failed with status {}",status.as_u16())));
    }
    let mut authorization=found.ok_or_else(||OAuthRequestError::Invalid("Authorization server metadata not found".into()))?;
    for field in ["authorization_endpoint","token_endpoint"] {
        if authorization.get(field).and_then(Value::as_str).is_none_or(str::is_empty){return Err(OAuthRequestError::Invalid(format!("Authorization server metadata is missing {field}")));}
    }
    for field in ["authorization_endpoint","token_endpoint","registration_endpoint"] {
        let Some(endpoint)=authorization.get(field).and_then(Value::as_str).filter(|value|!value.is_empty()).map(str::to_owned) else{continue;};
        let parsed=url::Url::parse(&endpoint)?;
        if require_https&&parsed.scheme()!="https"{return Err(OAuthRequestError::Invalid(format!("{field} must use https")));}
        authorization[field]=Value::String(parsed.to_string());
    }
    Ok(OAuthServerInfo {authorization_server_url:issuer,authorization_server_metadata:authorization,resource_metadata})
}
pub struct BeginAuthResult {pub authorized:bool,pub authorization_url:Option<url::Url>}
pub async fn begin_authorization(provider:&mut McpOAuthProvider,client:&reqwest::Client)->Result<BeginAuthResult,OAuthRequestError> {begin_authorization_inner(provider,client,false).await}
/// Like `begin_authorization` but never short-circuits on stored tokens.
pub async fn begin_authorization_forced(provider:&mut McpOAuthProvider,client:&reqwest::Client)->Result<BeginAuthResult,OAuthRequestError> {begin_authorization_inner(provider,client,true).await}
async fn begin_authorization_inner(provider:&mut McpOAuthProvider,client:&reqwest::Client,force:bool)->Result<BeginAuthResult,OAuthRequestError> {
    let info=discover(provider,client).await?;
    assert_s256_supported(Some(&info.authorization_server_metadata),&provider.store.server_name).map_err(OAuthRequestError::Flow)?;
    if !force && provider.tokens(chrono::Utc::now().timestamp_millis() as f64)?.is_some(){return Ok(BeginAuthResult {authorized:true,authorization_url:None});}
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
    {let mut query=authorization.query_pairs_mut();query.append_pair("response_type","code").append_pair("client_id",client_id).append_pair("code_challenge_method","S256").append_pair("code_challenge",&challenge).append_pair("state",&state).append_pair("resource",&normalize_resource_url(&provider.store.server_url));
        if let Some(redirect)=&provider.redirect_url {query.append_pair("redirect_uri",redirect);}
        if let Some(scopes)=provider.effective_scopes() {query.append_pair("scope",&scopes.join(" "));}}
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
    let mut form=vec![("grant_type".into(),"authorization_code".into()),("code".into(),code.into()),("code_verifier".into(),provider.code_verifier()?)];
    add_resource_param(&mut form,&normalize_resource_url(&provider.store.server_url));
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
    let mut form=vec![("grant_type".into(),"client_credentials".into())];
    if let Some(scopes)=provider.effective_scopes() {form.push(("scope".into(),scopes.join(" ")));}
    add_resource_param(&mut form,&normalize_resource_url(&provider.store.server_url));
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
