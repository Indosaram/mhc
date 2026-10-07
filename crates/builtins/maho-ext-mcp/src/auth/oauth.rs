use super::oauth_errors::{OAuthFailureKind,OAuthFlowError};
use futures::FutureExt;
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
/// Pinned `discovery.ts::discoveryCache`: the process-local, never-persisted metadata cache.
static DISCOVERY_CACHE:std::sync::LazyLock<std::sync::Mutex<std::collections::BTreeMap<String,OAuthServerInfo>>> = std::sync::LazyLock::new(||std::sync::Mutex::new(std::collections::BTreeMap::new()));
/// Pinned `discovery.ts::pendingDiscovery`: in-flight coalescing, keyed like `discoveryCache`.
static PENDING_DISCOVERY:std::sync::LazyLock<std::sync::Mutex<std::collections::BTreeMap<String,PendingDiscovery>>> = std::sync::LazyLock::new(||std::sync::Mutex::new(std::collections::BTreeMap::new()));
/// Monotonic identity for a registered in-flight discovery, so a settled caller removes only the
/// entry it created and never a newer one for the same key.
static NEXT_DISCOVERY_ID:std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
type SharedDiscovery = futures::future::Shared<futures::future::BoxFuture<'static,Result<OAuthServerInfo,OAuthRequestError>>>;
struct PendingDiscovery {id:u64,future:SharedDiscovery}
/// Pinned `discovery.ts::discoverOAuthServerMetadata`: the resource URL is validated with
/// `parseHttpsUrl` on every call, then the walk is served from the process-local cache or the
/// in-flight `pendingDiscovery` coalescing, and a success is cached.
pub async fn discover(provider:&McpOAuthProvider,client:&reqwest::Client)->Result<OAuthServerInfo,OAuthRequestError> {
    let resource_key=parse_https_url(&provider.store.server_url,"Resource server URL",provider.require_https)?.to_string();
    discover_cached(&resource_key,provider.require_https,client).await
}
/// The pinned cache body (`discoveryCache` hit, else the shared `pendingDiscovery` promise, else
/// start the walk and register it), with the pinned `finally` clearing the pending entry.
async fn discover_cached(resource_key:&str,require_https:bool,client:&reqwest::Client)->Result<OAuthServerInfo,OAuthRequestError> {
    if let Some(cached)=DISCOVERY_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(resource_key).cloned() {return Ok(cached);}
    let (id,shared)={
        let mut pending=PENDING_DISCOVERY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let existing=pending.get(resource_key).map(|entry|(entry.id,entry.future.clone()));
        match existing {Some(pair)=>pair,None=>{
            let id=NEXT_DISCOVERY_ID.fetch_add(1,std::sync::atomic::Ordering::Relaxed);let key=resource_key.to_owned();let client=client.clone();
            let future:SharedDiscovery=async move {discover_uncached(&key,require_https,&client).await}.boxed().shared();
            pending.insert(resource_key.to_owned(),PendingDiscovery {id,future:future.clone()});(id,future)}}
    };
    let result=shared.await;
    {let mut pending=PENDING_DISCOVERY.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.get(resource_key).is_some_and(|current|current.id==id) {pending.remove(resource_key);}}
    if let Ok(info)=&result {DISCOVERY_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(resource_key.to_owned(),info.clone());}
    result
}
/// Pinned `discovery.ts::resetDiscoveryCache`.
pub fn reset_discovery_cache() {DISCOVERY_CACHE.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();PENDING_DISCOVERY.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();}
// `/.well-known/oauth-protected-resource` (RFC 9728) then
// `/.well-known/oauth-authorization-server` (RFC 8414, with the origin retry),
// mirroring the pinned `discoverOAuthServerMetadata` + `fetchAuthorizationServerMetadata`.
// The pinned `parseHttpsUrl` gate is enforced by `discover` through `provider.require_https`
// (production `true`); the deterministic loopback HTTP fixtures set that seam `false` explicitly.
async fn discover_uncached(resource_key:&str,require_https:bool,client:&reqwest::Client)->Result<OAuthServerInfo,OAuthRequestError> {
    let resource_url=url::Url::parse(resource_key)?;
    let metadata_url=well_known_url(&resource_url,"/.well-known/oauth-protected-resource")?;
    match fetch_metadata(client,&metadata_url).await? {
        MetadataFetch::Ok(metadata)=>{
            let issuer=parse_authorization_servers(&metadata).into_iter().next()
                .ok_or_else(||OAuthRequestError::Invalid("OAuth protected resource metadata missing authorization_servers".into()))?;
            fetch_authorization_server_metadata(client,&issuer,&metadata,require_https).await
        }
        MetadataFetch::Failed(404)=>fetch_authorization_server_metadata(client,resource_key,&Value::Null,require_https).await,
        MetadataFetch::Failed(status)=>Err(OAuthRequestError::Invalid(format!("OAuth protected resource metadata fetch failed ({status})"))),
    }
}

/// `new URL(path, base)`: an absolute well-known path resolved against the URL's origin.
fn well_known_url(base:&url::Url,path:&str)->Result<url::Url,url::ParseError> {
    let mut url=base.clone();url.set_path(path);url.set_query(None);url.set_fragment(None);Ok(url)
}

enum MetadataFetch {Ok(Value),Failed(u16)}

async fn fetch_metadata(client:&reqwest::Client,url:&url::Url)->Result<MetadataFetch,OAuthRequestError> {
    let response=client.get(url.clone()).header("accept","application/json").send().await?;
    if !response.status().is_success(){return Ok(MetadataFetch::Failed(response.status().as_u16()));}
    match response.json::<Value>().await {
        Ok(Value::Null)|Err(_)=>Err(OAuthRequestError::Invalid("OAuth metadata response is not valid JSON".into())),
        Ok(value)=>Ok(MetadataFetch::Ok(value)),
    }
}

fn parse_authorization_servers(metadata:&Value)->Vec<String> {
    metadata.get("authorization_servers").and_then(Value::as_array)
        .map(|servers|servers.iter().filter_map(Value::as_str).filter(|server|!server.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default()
}

async fn fetch_authorization_server_metadata(client:&reqwest::Client,issuer:&str,resource_metadata:&Value,require_https:bool)->Result<OAuthServerInfo,OAuthRequestError> {
    let issuer_url=parse_https_url(issuer,"Authorization server URL",require_https)?;
    let issuer_path=issuer_url.path().trim_end_matches('/').to_owned();
    let metadata_url=well_known_url(&issuer_url,&format!("/.well-known/oauth-authorization-server{issuer_path}"))?;
    let authorization=match fetch_metadata(client,&metadata_url).await? {
        MetadataFetch::Ok(json)=>json,
        MetadataFetch::Failed(status)=>{
            if status==404 && !issuer_path.is_empty() {
                let root_url=well_known_url(&issuer_url,"/.well-known/oauth-authorization-server")?;
                if let MetadataFetch::Ok(root)=fetch_metadata(client,&root_url).await? {root}
                else {return Err(OAuthRequestError::Invalid("OAuth authorization server metadata not found".into()));}
            } else if status==404 {
                return Err(OAuthRequestError::Invalid("OAuth authorization server metadata not found".into()));
            } else {
                return Err(OAuthRequestError::Invalid(format!("OAuth authorization server metadata fetch failed ({status})")));
            }
        }
    };
    let authorization=parse_metadata_fields(&authorization,require_https)?;
    Ok(OAuthServerInfo {authorization_server_url:issuer_url.to_string(),authorization_server_metadata:authorization,resource_metadata:resource_metadata.clone()})
}

/// Pinned `discovery.ts::parseHttpsUrl`: the production OAuth URL validator. `require_https` is
/// `true` in production; only the deterministic loopback HTTP fixtures set it `false`.
pub fn parse_https_url(value:&str,label:&str,require_https:bool)->Result<url::Url,OAuthRequestError> {
    let parsed=url::Url::parse(value)?;
    if require_https && parsed.scheme()!="https" {return Err(OAuthRequestError::Invalid(format!("{label} must use https")));}
    Ok(parsed)
}

/// Pinned `discovery.ts::parseMetadataFields`: the three endpoint URLs are validated with
/// `parseHttpsUrl` and stored in their `URL.toString()` normal form.
pub fn parse_metadata_fields(metadata:&Value,require_https:bool)->Result<Value,OAuthRequestError> {
    let authorization=parse_https_url(read_string_field(metadata,"authorization_endpoint")?,"authorization_endpoint",require_https)?;
    let token=parse_https_url(read_string_field(metadata,"token_endpoint")?,"token_endpoint",require_https)?;
    let mut normalized=metadata.clone();
    normalized["authorization_endpoint"]=Value::String(authorization.to_string());
    normalized["token_endpoint"]=Value::String(token.to_string());
    if let Some(registration)=metadata.get("registration_endpoint").and_then(Value::as_str).filter(|value|!value.is_empty()) {
        normalized["registration_endpoint"]=Value::String(parse_https_url(registration,"registration_endpoint",require_https)?.to_string());
    }
    Ok(normalized)
}

fn read_string_field<'a>(source:&'a Value,field:&str)->Result<&'a str,OAuthRequestError> {
    source.get(field).and_then(Value::as_str).filter(|value|!value.is_empty())
        .ok_or_else(||OAuthRequestError::Invalid(format!("OAuth metadata missing {field}")))
}
pub struct BeginAuthResult {pub authorized:bool,pub authorization_url:Option<url::Url>}
/// Pinned `provider.ts::login()` always runs the authorization-code redirect. The `/mcp auth-start`
/// entry keeps the idempotent stored-token short-circuit; the pinned step-up re-login
/// (`oauth-handler.ts::handleStepUpIfNeeded` -> `provider.login()`) must not, so it routes through
/// `begin_authorization_forced`.
pub async fn begin_authorization(provider:&mut McpOAuthProvider,client:&reqwest::Client)->Result<BeginAuthResult,OAuthRequestError> {
    begin_authorization_with(provider,client,false).await
}
/// Pinned `provider.ts::login()` re-login: run the authorization flow even when a stored token is
/// still fresh, so the escalated scopes actually reach the authorization server.
pub async fn begin_authorization_forced(provider:&mut McpOAuthProvider,client:&reqwest::Client)->Result<BeginAuthResult,OAuthRequestError> {
    begin_authorization_with(provider,client,true).await
}
async fn begin_authorization_with(provider:&mut McpOAuthProvider,client:&reqwest::Client,force:bool)->Result<BeginAuthResult,OAuthRequestError> {
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
    let resource=provider.resource_indicator();
    {let mut query=authorization.query_pairs_mut();query.append_pair("response_type","code").append_pair("client_id",client_id).append_pair("code_challenge_method","S256").append_pair("code_challenge",&challenge).append_pair("state",&state).append_pair("resource",&resource);
        if let Some(redirect)=&provider.redirect_url {query.append_pair("redirect_uri",redirect);}
        let scopes=provider.effective_scopes();if !scopes.is_empty() {query.append_pair("scope",&scopes.join(" "));}}
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
    let mut form=vec![("grant_type".into(),"authorization_code".into()),("code".into(),code.into()),("code_verifier".into(),provider.code_verifier()?), ("resource".into(),provider.resource_indicator())];
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
    let mut form=vec![("grant_type".into(),"client_credentials".into()),("resource".into(),provider.resource_indicator())];
    let scopes=provider.effective_scopes();if !scopes.is_empty() {form.push(("scope".into(),scopes.join(" ")));}
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
