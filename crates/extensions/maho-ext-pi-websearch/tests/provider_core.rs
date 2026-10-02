use maho_ext_pi_websearch::websearch::{types::*,providers::{shared::*,exa::ExaProvider,tavily::TavilyProvider}};
fn config(provider:SearchProvider)->SearchProviderConfig{serde_json::from_value(serde_json::json!({"provider":provider})).unwrap_or_else(|e|panic!("provider fixture: {e}"))}
fn request(query:&str,max:f64)->SearchRequest{SearchRequest{query:query.into(),max_results:max,allowed_domains:None,blocked_domains:None}}
#[test]
fn duckduckgo_decodes_redirect_and_strips_snippet_tags(){
    use maho_ext_pi_websearch::websearch::providers::duckduckgo_html::DuckDuckGoHtmlProvider;
    let data=serde_json::json!({"html":r#"<a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.test%2Fa&amp;rut=x"><b>Docs</b> &amp; Help</a><a class="result__snippet"> Some <b>text</b> </a>"#});
    let results=DuckDuckGoHtmlProvider.normalize_response(data.as_object().expect("fixture object"));assert_eq!(results.len(),1);assert_eq!(results[0].url,"https://docs.test/a");assert_eq!(results[0].title,"Docs & Help");assert_eq!(results[0].snippet.as_deref(),Some("Some text"));
}
#[test]
fn responses_annotations_override_sources(){
    use maho_ext_pi_websearch::websearch::providers::openai_responses::normalize_responses_payload;
    let data=serde_json::json!({"output":[{"type":"web_search_call","action":{"sources":[{"url":"https://source.test"}]}},{"type":"message","content":[{"type":"output_text","text":"answer","annotations":[{"type":"url_citation","title":"Citation","url":"https://citation.test"}]}]}]});
    let results=normalize_responses_payload(data.as_object().expect("fixture object"),false);
    assert_eq!(results.len(),1);assert_eq!(results[0].url,"https://citation.test");assert_eq!(results[0].snippet.as_deref(),Some("answer"));
}
#[test]
fn responses_text_urls_are_unique_before_punctuation_cleanup(){
    use maho_ext_pi_websearch::websearch::providers::openai_responses::normalize_responses_payload;
    let data=serde_json::json!({"output":[{"type":"message","content":[{"type":"output_text","text":"https://a.test., https://a.test., https://a.test;"}]}]});
    let results=normalize_responses_payload(data.as_object().expect("fixture object"),false);
    assert_eq!(results.len(),2);assert!(results.iter().all(|item|item.url=="https://a.test"));
}
#[test]
fn xai_citations_fallback_and_allowlist_limit(){
    use maho_ext_pi_websearch::websearch::providers::xai::XaiProvider;
    let config=config(SearchProvider::Xai);let request=request("docs",5.0);
    let built=XaiProvider.build_request(&BuildContext{config:&config,request:&request,max_results:5.0,allowed_domains:Some(vec!["a".into(),"b".into(),"c".into(),"d".into(),"e".into(),"f".into()]),blocked_domains:Some(vec!["blocked".into()])});
    let body=built.body.expect("request body");assert_eq!(body["tools"][0]["filters"],serde_json::json!({"allowed_domains":["a","b","c","d","e"]}));
    let data=serde_json::json!({"citations":["https://citation.test"]});let results=XaiProvider.normalize_response(data.as_object().expect("fixture object"));assert_eq!(results[0].url,"https://citation.test");
}
#[test]
fn anthropic_search_result_prefers_page_age_over_joined_text(){
    use maho_ext_pi_websearch::websearch::providers::anthropic::AnthropicProvider;
    let value=serde_json::json!({"content":[{"type":"text","text":"first"},{"type":"text","text":"second"},{"type":"web_search_tool_result","content":[{"title":"A","url":"https://a.test","page_age":"today"},{"title":"B","url":"https://b.test"}]}]});
    let results=AnthropicProvider.normalize_response(value.as_object().expect("fixture object"));
    assert_eq!(results[0].snippet.as_deref(),Some("today"));assert_eq!(results[1].snippet.as_deref(),Some("first\nsecond"));
}
#[test]
fn google_cse_clamps_to_ten_and_encodes_credentials(){
    use maho_ext_pi_websearch::websearch::providers::google_cse::GoogleCseProvider;
    let mut config=config(SearchProvider::GoogleCse);config.api_key=Some("fixture key".into());config.search_engine_id=Some("cx".into());let request=request("docs",20.0);
    let built=GoogleCseProvider.build_request(&BuildContext{config:&config,request:&request,max_results:20.0,allowed_domains:None,blocked_domains:None}).expect("fixture URL");
    assert_eq!(built.url,"https://customsearch.googleapis.com/customsearch/v1?q=docs&key=fixture+key&cx=cx&num=10");
}
#[test]
fn brave_replaces_existing_query_and_maps_domains(){
    use maho_ext_pi_websearch::websearch::providers::brave::BraveProvider;
    let mut config=config(SearchProvider::Brave);config.base_url=Some("https://example.com/search?q=old&keep=1&q=duplicate".into());
    let request=request("node fetch",2.0);
    let built=BraveProvider.build_request(&BuildContext{config:&config,request:&request,max_results:2.0,allowed_domains:Some(vec!["nodejs.org".into()]),blocked_domains:None}).expect("fixture URL");
    assert_eq!(built.url,"https://example.com/search?q=node+fetch+site%3Anodejs.org&keep=1&count=2");
}
#[test]
fn perplexity_chat_preserves_allowlist_priority(){
    use maho_ext_pi_websearch::websearch::providers::perplexity::PerplexityProvider;
    let mut config=config(SearchProvider::Perplexity);config.model=Some("sonar-pro".into());
    let request=request("docs",25.0);
    let built=PerplexityProvider.build_request(&BuildContext{config:&config,request:&request,max_results:25.0,allowed_domains:Some(vec![]),blocked_domains:Some(vec!["blocked.example".into()])});
    assert_eq!(serde_json::to_value(built.body).unwrap(),serde_json::json!({"model":"sonar-pro","messages":[{"role":"user","content":"docs"}],"search_domain_filter":[]}));
}
#[test]
fn z_ai_falls_back_when_chat_results_are_invalid(){
    use maho_ext_pi_websearch::websearch::providers::z_ai::ZAiProvider;
    let data=serde_json::json!({"web_search":[{"title":"invalid"}],"search_result":[{"title":"Z Result","link":"https://z.example.com","content":"Z snippet","media":"Example"}]});
    let results=ZAiProvider.normalize_response(data.as_object().unwrap());
    assert_eq!(results.len(),1);assert_eq!(results[0].source.as_deref(),Some("Example"));
}
#[test]
fn serper_maps_domain_filters_and_clamps_results(){
    use maho_ext_pi_websearch::websearch::providers::serper::SerperProvider;
    let config=config(SearchProvider::Serper);let request=request("docs",99.0);
    let built=SerperProvider.build_request(&BuildContext{config:&config,request:&request,max_results:99.0,allowed_domains:Some(vec!["example.com".into()]),blocked_domains:None});
    assert_eq!(built.body.unwrap(),serde_json::json!({"q":"docs site:example.com","num":20.0}).as_object().unwrap().clone());
}
#[test]
fn kimi_preserves_summary_precedence(){
    use maho_ext_pi_websearch::websearch::providers::kimi::KimiProvider;
    let data=serde_json::json!({"search_results":[{"title":"Docs","url":"https://example.com","summary":"","content":"fallback"},{"title":"Missing URL"}]});
    let results=KimiProvider.normalize_response(data.as_object().unwrap());
    assert_eq!(results.len(),1);assert_eq!(results[0].snippet,None);
}
#[test]fn exa_key_and_body(){let mut config=config(SearchProvider::Exa);config.api_key=Some("exa-test".into());let request=request("pi extension",4.0);let built=ExaProvider.build_request(&BuildContext{config:&config,request:&request,max_results:4.0,allowed_domains:None,blocked_domains:None});assert_eq!(built.url,"https://api.exa.ai/search");assert_eq!(built.init.headers["x-api-key"],"exa-test");assert_eq!(serde_json::to_value(built.body).unwrap(),serde_json::json!({"query":"pi extension","numResults":4.0}));}
#[test]fn tavily_bearer_and_body(){let mut config=config(SearchProvider::Tavily);config.api_key=Some("tvly-test".into());let request=request("current docs",5.0);let built=TavilyProvider.build_request(&BuildContext{config:&config,request:&request,max_results:5.0,allowed_domains:None,blocked_domains:None});assert_eq!(built.init.headers["Authorization"],"Bearer tvly-test");assert_eq!(serde_json::to_value(built.body).unwrap(),serde_json::json!({"query":"current docs","max_results":5.0}));}
#[test]fn request_narrows_allowlist(){let mut config=config(SearchProvider::Exa);config.allowed_domains=Some(vec!["docs.example.com".into(),"api.example.com".into()]);let mut request=request("sdk docs",3.0);request.allowed_domains=Some(vec!["api.example.com".into(),"other.example.com".into()]);let filters=resolve_domain_filters(&config,&request);assert_eq!(filters.allowed_domains,Some(vec!["api.example.com".into()]));}
#[test]fn request_blocklist_narrows_allowlist(){let mut config=config(SearchProvider::Exa);config.allowed_domains=Some(vec!["docs.example.com".into(),"api.example.com".into()]);let mut request=request("sdk docs",3.0);request.blocked_domains=Some(vec!["docs.example.com".into()]);assert_eq!(resolve_domain_filters(&config,&request).allowed_domains,Some(vec!["api.example.com".into()]));}
#[test]fn blocked_not_reintroduced(){let mut config=config(SearchProvider::Brave);config.blocked_domains=Some(vec!["blocked.example.com".into()]);let mut request=request("node fetch",2.0);request.allowed_domains=Some(vec!["blocked.example.com".into(),"nodejs.org".into()]);assert_eq!(resolve_domain_filters(&config,&request).allowed_domains,Some(vec!["nodejs.org".into()]));}
