use std::collections::BTreeMap;
use maho_ai::{types::Model,utils::chatgpt_subscription_auth::extract_chatgpt_subscription_account_id};
use sha2::{Digest,Sha256};
use url::Url;
use crate::openai_remote_schema::OpenAiRemoteCompactionOrigin;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenAiRemoteCompactionIdentity {pub provider:String,pub api:String}
pub fn parse_openai_remote_compaction_identity(provider:&str,api:&str)->Option<OpenAiRemoteCompactionIdentity> {
    if (!provider.is_empty() && api=="openai-responses") || (provider=="chatgpt-subscription" && api=="openai-codex-responses") {Some(OpenAiRemoteCompactionIdentity {provider:provider.into(),api:api.into()})} else {None}
}
fn codex(model:&Model)->bool {model.api=="openai-codex-responses"}
fn base_url(model:&Model)->&str {if model.base_url.is_empty() {if codex(model) {"https://chatgpt.com/backend-api"} else {"https://api.openai.com/v1"}} else {&model.base_url}}
pub fn is_openai_remote_compaction_model(model:Option<&Model>)->bool {
    let Some(model)=model else {return false;};
    if parse_openai_remote_compaction_identity(&model.provider,&model.api).is_none() {return false;}
    if model.api=="openai-responses" && model.provider!="openai" && !model.compat.as_ref().is_some_and(|c|c.0.get("supportsRemoteCompactionV2")==Some(&serde_json::Value::Bool(true))) {return false;}
    if !codex(model) {return true;}
    Url::parse(base_url(model)).is_ok_and(|u|(u.scheme()=="https" && u.host_str()==Some("chatgpt.com")) || matches!(u.host_str(),Some("127.0.0.1"|"[::1]"|"localhost")))
}
pub fn matches_openai_remote_compaction_identity(model:&Model,identity:&OpenAiRemoteCompactionIdentity)->bool {model.provider==identity.provider && model.api==identity.api}
pub fn openai_remote_compaction_identity(model:&Model)->OpenAiRemoteCompactionIdentity {OpenAiRemoteCompactionIdentity {provider:if codex(model) {"chatgpt-subscription".into()} else {model.provider.clone()},api:model.api.clone()}}
pub fn openai_remote_compaction_endpoint_path(model:&Model)->&'static str {if codex(model) {"codex/responses/compact"} else {"responses/compact"}}
pub fn openai_remote_compaction_endpoint_url(model:&Model)->Result<String,url::ParseError> {
    let base=base_url(model);let base=if base.ends_with('/') {base.into()} else {format!("{base}/")};
    Ok(Url::parse(&base)?.join(openai_remote_compaction_endpoint_path(model))?.to_string())
}
pub fn openai_remote_compaction_origin(model:&Model,headers:&BTreeMap<String,String>)->Option<OpenAiRemoteCompactionOrigin> {
    let mut url=Url::parse(base_url(model)).ok()?;
    let _=url.set_username("");let _=url.set_password(None);url.set_query(None);url.set_fragment(None);
    let path=url.path().trim_end_matches('/').to_owned();url.set_path(if path.is_empty() {"/"} else {&path});
    let trust_domain=url.origin().ascii_serialization();
    let endpoint=if url.path()=="/" {trust_domain.clone()} else {url.to_string().trim_end_matches('/').into()};
    if codex(model) && !headers.iter().any(|(k,v)|k.eq_ignore_ascii_case("chatgpt-account-id") && !v.is_empty()) {return None;}
    let mut material:Vec<_>=headers.iter().filter_map(|(k,v)| {
        let name=k.to_lowercase();
        if matches!(name.as_str(),"content-length"|"user-agent"|"request-id"|"x-client-request-id"|"x-request-id") || (codex(model) && name=="authorization") {None} else {Some(format!("{name}\0{}",v.trim()))}
    }).collect();
    if material.is_empty() {return None;}
    material.sort_by(|a,b|a.encode_utf16().cmp(b.encode_utf16()));
    let hash=Sha256::digest(material.join("\n").as_bytes());
    Some(OpenAiRemoteCompactionOrigin {endpoint,trust_domain,auth_tenant_fingerprint:format!("sha256:{hash:x}")})
}
pub fn create_openai_remote_compaction_headers(model:&Model,api_key:Option<&str>,additional:&BTreeMap<String,Option<String>>,session_id:Option<&str>,os_release:&str)->Option<BTreeMap<String,String>> {
    let mut headers:BTreeMap<_,_>=model.headers.iter().flat_map(|h|h.iter()).map(|(k,v)|(k.to_lowercase(),v.trim().to_owned())).collect();
    for (k,v) in additional {let k=k.to_lowercase();if let Some(v)=v {headers.insert(k,v.trim().into());} else {headers.remove(&k);}}
    headers.insert("content-type".into(),"application/json".into());
    if codex(model) && let Some(key)=api_key.filter(|k|!k.is_empty()) {
        headers.insert("authorization".into(),format!("Bearer {key}"));
        if let Some(account)=extract_chatgpt_subscription_account_id(key) {headers.insert("chatgpt-account-id".into(),account);} else {headers.remove("chatgpt-account-id");}
    } else if !headers.contains_key("authorization") && let Some(key)=api_key.filter(|k|!k.is_empty()) {headers.insert("authorization".into(),format!("Bearer {key}"));}
    if !headers.contains_key("authorization") {return None;}
    if codex(model) {
        headers.insert("originator".into(),"senpi".into());headers.insert("user-agent".into(),format!("senpi (linux {os_release}; x64)"));headers.insert("openai-beta".into(),"responses=experimental".into());headers.insert("accept".into(),"text/event-stream".into());
        if let Some(id)=session_id.filter(|id|!id.is_empty()) {headers.insert("session-id".into(),id.into());headers.insert("x-client-request-id".into(),id.into());}
    }
    Some(headers)
}
