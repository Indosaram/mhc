use std::collections::BTreeMap;
use serde_json::{Map,Value};
use super::{shared::*,super::{types::*,provider_endpoints::provider_url}};
pub struct BraveProvider;
impl BraveProvider{
    pub fn build_request(&self,context:&BuildContext<'_>)->Result<BuiltSearchRequest,url::ParseError>{
        let mut url=url::Url::parse(provider_url(context.config.provider,context.config.base_url.as_deref()))?;
        let query=append_domain_filters(&context.request.query,context.allowed_domains.as_deref(),context.blocked_domains.as_deref());
        let count=clamp(context.max_results,1.0,20.0).to_string();
        let mut pairs=url.query_pairs().into_owned().collect::<Vec<_>>();
        for (key,value) in [("q",query),("count",count)]{
            if let Some(index)=pairs.iter().position(|(name,_)|name==key){pairs[index].1=value;pairs=pairs.into_iter().enumerate().filter(|(position,(name,_))|*position==index||name!=key).map(|(_,pair)|pair).collect();}else{pairs.push((key.into(),value));}
        }
        url.query_pairs_mut().clear().extend_pairs(pairs);
        Ok(BuiltSearchRequest{url:url.into(),init:SearchRequestInit{method:HttpMethod::Get,headers:BTreeMap::from([("Accept".into(),"application/json".into()),("X-Subscription-Token".into(),context.config.api_key.clone().unwrap_or_default())])},body:None})
    }
    pub fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>{
        let web=get_object(data.get("web"));
        collect(get_array(web.and_then(|web|web.get("results"))).iter().map(|raw|{let item=get_object(Some(raw))?;result(get_string(item.get("title")),get_string(item.get("url")),get_string(item.get("description")),None,None)}).collect(),50)
    }
}
