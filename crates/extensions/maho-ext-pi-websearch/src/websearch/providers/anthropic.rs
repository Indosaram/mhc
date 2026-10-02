use std::collections::BTreeMap;
use serde_json::{Map,Value,json};
use super::{shared::*,super::{types::*,provider_endpoints::provider_url}};
pub struct AnthropicProvider;
impl ProviderModule for AnthropicProvider{
    fn build_request(&self,context:&BuildContext<'_>)->BuiltSearchRequest{
        let mut search=Map::from_iter([("type".into(),json!("web_search_20250305")),("name".into(),json!("web_search")),("max_uses".into(),json!(8))]);
        if let Some(domains)=&context.allowed_domains{search.insert("allowed_domains".into(),json!(domains));}
        if let Some(domains)=&context.blocked_domains{search.insert("blocked_domains".into(),json!(domains));}
        BuiltSearchRequest{url:provider_url(context.config.provider,context.config.base_url.as_deref()).into(),init:SearchRequestInit{method:HttpMethod::Post,headers:content_headers(Some(&BTreeMap::from([("x-api-key".into(),context.config.api_key.clone().unwrap_or_default()),("anthropic-version".into(),"2023-06-01".into())])))},body:Some(Map::from_iter([("model".into(),json!(context.config.model.as_deref().unwrap_or("claude-sonnet-4-5-20250929"))),("max_tokens".into(),json!(1024)),("messages".into(),json!([{ "role":"user","content":context.request.query }])),("tools".into(),json!([search]))]))}
    }
    fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>{
        let content=get_array(data.get("content"));
        let text=content.iter().filter_map(|raw|get_object(Some(raw)).and_then(|item|get_string(item.get("text")))).collect::<Vec<_>>().join("\n");
        let mut items=Vec::new();
        for raw in content{let Some(item)=get_object(Some(raw))else{continue;};if get_string(item.get("type"))!=Some("web_search_tool_result"){continue;}
            for raw in get_array(item.get("content")){let found=get_object(Some(raw));items.push(result(get_string(found.and_then(|item|item.get("title"))),get_string(found.and_then(|item|item.get("url"))),get_string(found.and_then(|item|item.get("page_age"))).or(Some(text.as_str())),None,None));}
        }
        collect(items,50)
    }
}
