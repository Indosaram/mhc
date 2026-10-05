use std::{collections::{BTreeMap,BTreeSet},future::Future,pin::Pin};
use sha2::{Digest,Sha256};
use super::{types::*,provider_endpoints::is_allowed_provider_base_url};
#[derive(Clone,Debug)]
pub struct NativeModelInfo{pub provider:String,pub id:String,pub base_url:String}
pub enum NativeAuthResult{Success{api_key:Option<String>,headers:Option<BTreeMap<String,String>>},Failure{error:String}}
pub type NativeAuthFuture<'a>=Pin<Box<dyn Future<Output=Result<NativeAuthResult,String>>+Send+'a>>;
pub trait NativeModelRegistry:Send+Sync{
    fn get_api_key_and_headers<'a>(&'a self,model:&'a NativeModelInfo)->NativeAuthFuture<'a>;
    fn get_available(&self)->Option<Vec<NativeModelInfo>>{None}
}
pub struct ContextModelRegistry(pub std::sync::Arc<dyn maho_ext_api::ModelRegistry>);
impl NativeModelRegistry for ContextModelRegistry {
    fn get_api_key_and_headers<'a>(&'a self,model:&'a NativeModelInfo)->NativeAuthFuture<'a>{Box::pin(async move{
        let Some(model)=self.0.find(&model.provider,&model.id)else{return Ok(NativeAuthResult::Failure{error:"Model is unavailable".into()});};
        match self.0.get_api_key_and_headers(&model).await {
            Ok(result)=>Ok(NativeAuthResult::Success{api_key:result.auth.api_key,headers:result.auth.headers.map(|headers|headers.into_iter().filter_map(|(key,value)|value.map(|value|(key,value))).collect())}),
            Err(error)=>Ok(NativeAuthResult::Failure{error:error.to_string()}),
        }
    })}
    fn get_available(&self)->Option<Vec<NativeModelInfo>>{Some(self.0.get_available().into_iter().map(|model|NativeModelInfo{provider:model.provider,id:model.id,base_url:model.base_url}).collect())}
}
fn native_mapping(model:&NativeModelInfo)->Option<(SearchProvider,&'static str)>{
    match model.provider.as_str(){
        "openai" if ["gpt-4o","gpt-4.1","gpt-5"].iter().any(|prefix|model.id.starts_with(prefix))&&!model.id.contains("codex")=>Some((SearchProvider::Openai,"responses")),
        "anthropic" if model.id.starts_with("claude-")=>Some((SearchProvider::Anthropic,"messages")),
        "xai" if model.id.starts_with("grok-")=>Some((SearchProvider::Xai,"responses")),
        "perplexity" if model.id.starts_with("sonar")=>Some((SearchProvider::Perplexity,"chat/completions")),
        "z-ai"|"zai" if model.id.starts_with("glm-")=>Some((SearchProvider::Zai,"chat/completions")),
        "kimi-coding"=>Some((SearchProvider::Kimi,"search")),
        "openrouter"=>{let (provider,id)=model.id.split_once('/')?;if provider.is_empty()||provider=="openrouter"{return None;}native_mapping(&NativeModelInfo{provider:provider.into(),id:id.into(),base_url:model.base_url.clone()})},
        _=>None,
    }
}
fn build_endpoint_url(base_url:&str,resource:&str)->String{
    let Ok(mut configured)=url::Url::parse(base_url)else{return base_url.into();};
    let path=configured.path().trim_end_matches('/');let suffix=format!("/{resource}");
    let versioned=path.rsplit_once('/').is_some_and(|(_,last)|last.strip_prefix('v').is_some_and(|digits|!digits.is_empty()&&digits.bytes().all(|byte|byte.is_ascii_digit())));
    let path=if path.ends_with(&suffix){path.into()}else if versioned{format!("{path}{suffix}")}else{format!("{path}/v1{suffix}")};
    configured.set_path(&path);configured.set_fragment(None);configured.into()
}
pub async fn build_native_entry(model:Option<&NativeModelInfo>,registry:Option<&dyn NativeModelRegistry>,id:&str)->Result<Option<SearchProviderEntry>,String>{
    let (Some(model),Some(registry))=(model,registry)else{return Ok(None);};
    let Some((provider,resource))=native_mapping(model)else{return Ok(None);};let base_url=build_endpoint_url(&model.base_url,resource);
    if !is_allowed_provider_base_url(&base_url){return Ok(None);}
    let NativeAuthResult::Success{api_key:Some(api_key),..}=registry.get_api_key_and_headers(model).await?else{return Ok(None);};if api_key.is_empty(){return Ok(None);}
    Ok(Some(SearchProviderEntry{config:SearchProviderConfig{provider,id:Some(id.into()),api_key:Some(api_key),base_url:Some(base_url),model:Some(model.id.clone()),search_engine_id:None,max_results:None,codex_mode:None,search_context_size:None,allowed_domains:None,blocked_domains:None,user_location:None,timeout_ms:None},priority:Some(-1.0),weight:None}))
}
fn native_route_key(model:&NativeModelInfo)->Option<String>{
    let (provider,resource)=native_mapping(model)?;let base=build_endpoint_url(&model.base_url,resource);if !is_allowed_provider_base_url(&base){return None;}
    Some(format!("{}|{base}",provider_name(provider)))
}
pub(crate) fn provider_name(provider:SearchProvider)->&'static str{
    match provider{SearchProvider::Openai=>"openai",SearchProvider::Anthropic=>"anthropic",SearchProvider::Xai=>"xai",SearchProvider::Perplexity=>"perplexity",SearchProvider::Zai=>"z-ai",SearchProvider::Kimi=>"kimi",SearchProvider::Exa=>"exa",SearchProvider::Tavily=>"tavily",SearchProvider::Brave=>"brave",SearchProvider::DuckduckgoHtml=>"duckduckgo-html",SearchProvider::Serper=>"serper",SearchProvider::GoogleCse=>"google-cse",SearchProvider::Codex=>"codex"}
}
pub async fn build_native_entries(model:Option<&NativeModelInfo>,registry:Option<&dyn NativeModelRegistry>)->Result<Vec<SearchProviderEntry>,String>{
    let Some(registry)=registry else{return Ok(Vec::new());};let mut entries=Vec::new();let mut seen=BTreeSet::new();
    if let Some(key)=model.and_then(native_route_key){seen.insert(key);if let Some(entry)=build_native_entry(model,Some(registry),"native").await?{entries.push(entry);}}
    let Some(available)=registry.get_available()else{return Ok(entries);};
    for model in available{
        let Some(key)=native_route_key(&model)else{continue;};if !seen.insert(key.clone()){continue;}
        if let Some(mut entry)=build_native_entry(Some(&model),Some(registry),"native-discovered").await?{
            let fingerprint=format!("{:x}",Sha256::digest(key.as_bytes()));entry.config.id=Some(format!("native-{}-{}",provider_name(entry.config.provider),&fingerprint[..16]));entries.push(entry);
        }
    }
    Ok(entries)
}
