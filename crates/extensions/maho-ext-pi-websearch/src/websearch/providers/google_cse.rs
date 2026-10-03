use std::collections::BTreeMap;
use serde_json::{Map,Value};
use super::{shared::*,super::{types::*,provider_endpoints::provider_url}};
pub struct GoogleCseProvider;
impl GoogleCseProvider{
    pub fn build_request(&self,context:&BuildContext<'_>)->Result<BuiltSearchRequest,url::ParseError>{
        let mut url=url::Url::parse(provider_url(context.config.provider,context.config.base_url.as_deref()))?;
        let query=append_domain_filters(&context.request.query,context.allowed_domains.as_deref(),context.blocked_domains.as_deref());
        let mut pairs=url.query_pairs().into_owned().collect::<Vec<_>>();
        for (key,value) in [("q",query),("key",context.config.api_key.clone().unwrap_or_default()),("cx",context.config.search_engine_id.clone().unwrap_or_default()),("num",clamp(context.max_results,1.0,10.0).to_string())]{
            if let Some(index)=pairs.iter().position(|(name,_)|name==key){pairs[index].1=value;pairs=pairs.into_iter().enumerate().filter(|(position,(name,_))|*position==index||name!=key).map(|(_,pair)|pair).collect();}else{pairs.push((key.into(),value));}
        }
        url.query_pairs_mut().clear().extend_pairs(pairs);
        Ok(BuiltSearchRequest{url:url.into(),init:SearchRequestInit{method:HttpMethod::Get,headers:BTreeMap::from([("Accept".into(),"application/json".into())])},body:None})
    }
    pub fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>{collect(get_array(data.get("items")).iter().map(|raw|{let item=get_object(Some(raw))?;result(get_string(item.get("title")),get_string(item.get("link")),get_string(item.get("snippet")),None,None)}).collect(),50)}
}
