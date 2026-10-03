use std::collections::BTreeMap;
use serde_json::{Map,Value,json};
use super::{shared::*,super::{types::*,provider_endpoints::provider_url}};
pub struct OpenAiResponsesProvider;
pub fn build_responses_request(context:&BuildContext<'_>)->BuiltSearchRequest{
    let mut tool=Map::from_iter([("type".into(),json!("web_search")),("external_web_access".into(),json!(context.config.codex_mode.unwrap_or(CodexSearchMode::Live)==CodexSearchMode::Live))]);
    if let Some(size)=context.config.search_context_size{tool.insert("search_context_size".into(),json!(size));}
    if let Some(domains)=&context.allowed_domains{tool.insert("filters".into(),json!({"allowed_domains":domains}));}
    if let Some(location)=&context.config.user_location{let mut fields=Map::from_iter([("type".into(),json!("approximate"))]);for (key,value) in [("country",&location.country),("region",&location.region),("city",&location.city),("timezone",&location.timezone)]{if let Some(value)=value{fields.insert(key.into(),json!(value));}}tool.insert("user_location".into(),json!(fields));}
    let query=append_domain_filters(&context.request.query,None,context.blocked_domains.as_deref());
    let input=format!("Find web pages matching any of these search terms or quoted phrases. If the query contains OR, search each alternative independently. Return only relevant source URLs, one per line. Query: {query}");
    BuiltSearchRequest{url:provider_url(context.config.provider,context.config.base_url.as_deref()).into(),init:SearchRequestInit{method:HttpMethod::Post,headers:content_headers(Some(&BTreeMap::from([("Authorization".into(),format!("Bearer {}",context.config.api_key.as_deref().unwrap_or("")))])))},body:Some(Map::from_iter([("model".into(),json!(context.config.model.as_deref().unwrap_or("gpt-5.5"))),("input".into(),json!(input)),("tools".into(),json!([tool])),("include".into(),json!(["web_search_call.action.sources"])),("tool_choice".into(),json!("required"))]))}
}
pub fn normalize_responses_payload(data:&Map<String,Value>,citations_fallback:bool)->Vec<SearchResultItem>{
    let output=get_array(data.get("output"));let mut sources=Vec::new();
    for raw in output{let Some(item)=get_object(Some(raw))else{continue;};if get_string(item.get("type"))!=Some("web_search_call"){continue;}let action=get_object(item.get("action"));for raw in get_array(action.and_then(|action|action.get("sources"))){let url=get_string(get_object(Some(raw)).and_then(|item|item.get("url")));sources.push(result(url,url,None,None,None));}}
    let message=output.iter().filter_map(|raw|get_object(Some(raw))).find(|item|get_string(item.get("type"))==Some("message"));
    let content=get_array(message.and_then(|message|message.get("content"))).iter().filter_map(|raw|get_object(Some(raw))).find(|item|get_string(item.get("type"))==Some("output_text"));let text=get_string(content.and_then(|content|content.get("text")));
    let annotations=collect(get_array(content.and_then(|content|content.get("annotations"))).iter().map(|raw|{let item=get_object(Some(raw))?;if get_string(item.get("type"))!=Some("url_citation"){return None;}result(get_string(item.get("title")),get_string(item.get("url")),text,None,None)}).collect(),50);
    if !annotations.is_empty(){return annotations;}
    let mut sources=collect(sources,50);if !sources.is_empty(){for source in &mut sources{if source.snippet.is_none(){source.snippet=text.map(str::to_owned);}}return sources;}
    if let Some(text)=text.filter(|text|!text.is_empty()){
        let mut urls=Vec::new();let mut rest=text;
        while let Some(start)=rest.find("http://").into_iter().chain(rest.find("https://")).min(){rest=&rest[start..];let end=rest.char_indices().find(|(_,ch)|matches!(ch,')'|']'|'}'|'>'|'"')||matches!(ch,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')).map_or(rest.len(),|(index,_)|index);let url=&rest[..end];if url.len()>if url.starts_with("https"){8}else{7}&&!urls.contains(&url){urls.push(url);}rest=&rest[end..];}
        let items=collect(urls.into_iter().map(|url|{let clean=url.trim_end_matches(['.',',',';',':']);result(Some(clean),Some(clean),Some(text),None,None)}).collect(),50);if !items.is_empty(){return items;}
    }
    if !citations_fallback{return annotations;}
    collect(get_array(data.get("citations")).iter().map(|raw|{let url=get_string(Some(raw));result(url,url,text,None,None)}).collect(),50)
}
impl ProviderModule for OpenAiResponsesProvider{fn build_request(&self,context:&BuildContext<'_>)->BuiltSearchRequest{build_responses_request(context)}fn normalize_response(&self,data:&Map<String,Value>)->Vec<SearchResultItem>{normalize_responses_payload(data,false)}}
