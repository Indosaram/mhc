use serde_json::{Value,json};
use super::{types::{SearchRequest,WebsearchConfig,RoutingStrategy},search::{SearchRoutingState,create_search_routing_state}};
pub fn search_details_value(details:&super::types::SearchDetails)->Value {
    let mut value=json!({"provider":details.provider.as_str(),"query":details.query,"results":details.results.iter().map(|result| {
        let mut value=json!({"title":result.title,"url":result.url});
        for (key,text) in [("snippet",&result.snippet),("source",&result.source),("publishedAt",&result.published_at)] { if let Some(text)=text { value[key]=json!(text); } }
        if let Some(score)=result.score { value["score"]=json!(score); } value
    }).collect::<Vec<_>>(),"durationMs":details.duration_ms,"truncated":details.truncated});
    for (key,text) in [("entryId",&details.entry_id),("answer",&details.answer),("error",&details.error)] { if let Some(text)=text { value[key]=json!(text); } }
    if let Some(strategy)=details.strategy { value["strategy"]=json!(match strategy { RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first" }); }
    if let Some(attempts)=&details.attempts { value["attempts"]=json!(attempts.iter().map(|attempt| {
        let mut value=json!({"provider":attempt.provider.as_str(),"durationMs":attempt.duration_ms,"resultsCount":attempt.results_count});
        if let Some(id)=&attempt.entry_id { value["entryId"]=json!(id); }
        if let Some(error)=&attempt.error { value["error"]=json!(error); } value
    }).collect::<Vec<_>>()); } value
}
pub fn parameters()->Value { json!({"type":"object","properties":{"query":{"type":"string","minLength":2,"description":"The search query to use"},"allowed_domains":{"type":"array","items":{"type":"string"},"description":"Only include search results from these domains"},"blocked_domains":{"type":"array","items":{"type":"string"},"description":"Never include search results from these domains"}},"required":["query"],"additionalProperties":false}) }
pub fn request_from_arguments(query:String,allowed_domains:Option<Vec<String>>,blocked_domains:Option<Vec<String>>,config:&WebsearchConfig)->Result<SearchRequest,String> {
    if allowed_domains.as_ref().is_some_and(|domains|!domains.is_empty()) && blocked_domains.as_ref().is_some_and(|domains|!domains.is_empty()) { return Err("Error: Cannot specify both allowed_domains and blocked_domains in the same request".into()); }
    Ok(SearchRequest{query,max_results:config.providers.first().and_then(|entry|entry.config.max_results).unwrap_or(10.),allowed_domains,blocked_domains})
}
pub fn routing_key(config:&WebsearchConfig)->String {
    let strategy=match config.strategy { RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first" };
    format!("{strategy}:{}",config.providers.iter().map(|entry|entry.config.id.as_deref().unwrap_or(entry.config.provider.as_str())).collect::<Vec<_>>().join("|"))
}
pub fn sync_routing_state(config:&WebsearchConfig,key:&mut String,state:&mut Option<SearchRoutingState>) {
    let next=routing_key(config);
    if state.as_ref().is_none_or(|state|state.success_counts.len()!=config.providers.len()) || *key!=next { *state=Some(create_search_routing_state(config.providers.len())); *key=next; }
}
pub fn format_search_progress_text(query:&str,provider_labels:&[String],current_provider:Option<&str>)->String {
    let route=if let Some(provider)=current_provider.filter(|provider|!provider.is_empty()) { provider.into() } else if provider_labels.is_empty() { "configured providers".into() } else { provider_labels.join(" -> ") };
    format!("Searching \"{query}\" via {route}")
}
pub fn create_web_search_tool(get_config:std::sync::Arc<dyn Fn()->super::types::ConfigLoadResult+Send+Sync>)->maho_tools::definition::ToolDefinition {
    use maho_tools::definition::{ToolDefinition,ToolResult,ToolContent,ToolError};
    use std::sync::Arc;
    let routing=Arc::new(tokio::sync::Mutex::new((String::new(),None)));
    let mut tool=ToolDefinition::new("web_search","Search the web for current information and return source URLs for citation.",parameters(),Arc::new(move |call| {
        let get_config=get_config.clone(); let routing=routing.clone();
        Box::pin(async move {
            let query=call.params["query"].as_str().ok_or_else(||ToolError::Message("query is required".into()))?.to_owned();
            let allowed:Option<Vec<String>>=call.params.get("allowed_domains").map(|value|serde_json::from_value(value.clone())).transpose()?;
            let blocked:Option<Vec<String>>=call.params.get("blocked_domains").map(|value|serde_json::from_value(value.clone())).transpose()?;
            let error_result=|message:String,reason:Option<&str>| {
                let mut details=json!({"phase":"error","query":query,"error":message}); if let Some(reason)=reason { details["reason"]=json!(reason); }
                ToolResult{content:vec![ToolContent::text(message)],details:Some(details)}
            };
            if allowed.as_ref().is_some_and(|domains|!domains.is_empty()) && blocked.as_ref().is_some_and(|domains|!domains.is_empty()) { return Ok(error_result("Error: Cannot specify both allowed_domains and blocked_domains in the same request".into(),None)); }
            let config=match get_config() {
                super::types::ConfigLoadResult::Ok{config,..}=>config,
                super::types::ConfigLoadResult::Err{reason,message,..}=>return Ok(error_result(message,Some(match reason { super::types::ConfigLoadFailureReason::MissingConfig=>"missing_config",super::types::ConfigLoadFailureReason::InvalidConfig=>"invalid_config",super::types::ConfigLoadFailureReason::MissingApiKey=>"missing_api_key",super::types::ConfigLoadFailureReason::ProviderNativeBypass=>"provider_native_bypass" }))),
            };
            if config.auto {
                call.signal.check()?;
                if call.context.is_some() { return Err(ToolError::Message("Native search discovery requires ModelRegistry.getApiKeyAndHeaders binding".into())); }
            }
            let request=request_from_arguments(query.clone(),allowed.clone(),blocked.clone(),&config).map_err(ToolError::Message)?;
            let labels:Vec<_>=config.providers.iter().map(|entry|super::search::provider_entry_label(entry.config.provider.as_str(),entry.config.id.as_deref(),None)).collect();
            let mut progress=json!({"phase":"searching","query":query,"providerLabels":labels,"maxResults":request.max_results,"strategy":match config.strategy { RoutingStrategy::Priority=>"priority",RoutingStrategy::RoundRobin=>"round-robin",RoutingStrategy::FillFirst=>"fill-first" }});
            if let Some(allowed)=allowed { progress["allowedDomains"]=json!(allowed); }
            if let Some(blocked)=blocked { progress["blockedDomains"]=json!(blocked); }
            if let Some(update)=&call.on_update { update(ToolResult{content:vec![ToolContent::text(format_search_progress_text(&query,&labels,None))],details:Some(progress.clone())})?; }
            let listener=|provider:&str,attempts:&[super::types::SearchAttempt],routes:&[String]| {
                if let Some(update)=&call.on_update {
                    let mut details=progress.clone(); details["currentProvider"]=json!(provider); details["routeLabels"]=json!(routes);
                    let holder=super::types::SearchDetails{provider:super::types::SearchProvider::Exa,entry_id:None,query:String::new(),results:vec![],duration_ms:0.,truncated:false,strategy:None,attempts:Some(attempts.to_vec()),answer:None,error:None};
                    details["attempts"]=search_details_value(&holder)["attempts"].clone();
                    update(ToolResult{content:vec![ToolContent::text(format_search_progress_text(&query,&labels,Some(provider)))],details:Some(details)})?;
                }
                Ok(())
            };
            let mut routing=routing.lock().await; let (key,state)=&mut *routing; sync_routing_state(&config,key,state);
            let details=super::search::perform_search(&config,&request,Some(&call.signal),state.as_mut(),Some(&listener)).await?;
            Ok(ToolResult{content:vec![ToolContent::text(super::search::format_search_text(&details))],details:Some(search_details_value(&details))})
        })
    }));
    tool.label="Web Search".into(); tool.prompt_snippet=Some("Search the web for current information, documentation, news, or external facts.".into());
    tool.prompt_guidelines=Some(vec!["After using web_search, cite relevant returned URLs in the final answer.".into()]); tool
}
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{SearchProvider,SearchProviderConfig,SearchProviderEntry};
    fn config()->WebsearchConfig { WebsearchConfig{strategy:RoutingStrategy::Priority,fallback:true,auto:true,providers:vec![SearchProviderEntry{config:SearchProviderConfig::new(SearchProvider::Exa),priority:None,weight:None}]} }
    #[tokio::test] async fn upstream_fallback_emits_initial_and_per_attempt_progress() {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let server=async {
            for (status,body) in [("500 Internal Server Error","boom"),("200 OK",r#"{"results":[{"title":"Result","url":"https://example.com/a","text":"snippet"}]}"#)] {
                let (mut socket,_)=listener.accept().await.unwrap();let mut request=Vec::new();let mut chunk=[0;1024];
                loop {let count=socket.read(&mut chunk).await.unwrap();assert!(count>0);request.extend_from_slice(&chunk[..count]);if request.windows(4).any(|part|part==b"\r\n\r\n") {break}}
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        };
        let mut config=config();config.auto=false;
        config.providers[0].config.id=Some("primary".into());config.providers[0].config.base_url=Some(format!("http://{address}/search"));
        let mut backup=config.providers[0].clone();backup.config.id=Some("backup".into());config.providers.push(backup);
        let tool=create_web_search_tool(std::sync::Arc::new(move ||super::super::types::ConfigLoadResult::Ok{config:config.clone(),source:"fixture".into()}));
        let updates=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let observed=updates.clone();
        let client=async {(tool.execute)(maho_tools::definition::ToolCall{id:"progress",params:json!({"query":"attempt progress"}),signal:Default::default(),context:None,on_update:Some(std::sync::Arc::new(move |update| {observed.lock().unwrap().push(update.details.unwrap());Ok(())}))}).await.unwrap()};
        let (_,result)=tokio::time::timeout(std::time::Duration::from_secs(5),async {tokio::join!(server,client)}).await.unwrap();
        assert_eq!(result.details.unwrap()["entryId"],"backup");let updates=updates.lock().unwrap();assert_eq!(updates.len(),3);
        assert!(updates[0].get("currentProvider").is_none());assert_eq!(updates[0]["providerLabels"],json!(["exa/primary","exa/backup"]));
        assert_eq!(updates[1]["currentProvider"],"exa/primary");assert_eq!(updates[1]["attempts"],json!([]));
        assert_eq!(updates[2]["currentProvider"],"exa/backup");assert_eq!(updates[2]["attempts"].as_array().unwrap().len(),1);assert!(updates[2]["attempts"][0]["error"].as_str().unwrap().contains("HTTP 500"));assert_eq!(updates[2]["routeLabels"],json!(["exa/primary","exa/backup"]));
    }
    #[tokio::test] async fn native_executor_searches_http_and_emits_source_result_details() {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let address=listener.local_addr().unwrap();
        let server=async {
            let (mut socket,_)=listener.accept().await.unwrap(); let mut bytes=Vec::new(); let mut chunk=[0;1024];
            loop { let count=socket.read(&mut chunk).await.unwrap(); assert!(count>0); bytes.extend_from_slice(&chunk[..count]); if bytes.windows(4).any(|window|window==b"\r\n\r\n") { break; } }
            assert!(String::from_utf8_lossy(&bytes).starts_with("POST /search"));
            let body=r#"{"results":[{"title":"Docs","url":"https://example.com/docs","text":"documentation"}]}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        };
        let mut config=config(); config.providers[0].config.base_url=Some(format!("http://{address}/search"));
        let tool=create_web_search_tool(std::sync::Arc::new(move ||super::super::types::ConfigLoadResult::Ok{config:config.clone(),source:"test".into()}));
        let updates=std::sync::Arc::new(std::sync::Mutex::new(Vec::new())); let captured=updates.clone();
        let client=async {
            let result=(tool.execute)(maho_tools::definition::ToolCall{id:"native",params:json!({"query":"docs"}),signal:Default::default(),context:None,on_update:Some(std::sync::Arc::new(move |update| { captured.lock().unwrap().push(update); Ok(()) }))}).await.unwrap();
            let details=result.details.unwrap(); assert_eq!(details["query"],"docs"); assert_eq!(details["results"][0]["url"],"https://example.com/docs"); assert_eq!(details["attempts"][0]["resultsCount"],1);
        };
        tokio::time::timeout(std::time::Duration::from_secs(5),async { tokio::join!(server,client); }).await.unwrap();
        let updates=updates.lock().unwrap(); assert_eq!(updates.len(),2); assert_eq!(updates[0].details.as_ref().unwrap()["phase"],"searching"); assert_eq!(updates[1].details.as_ref().unwrap()["currentProvider"],"exa");
    }
    #[tokio::test] async fn native_executor_conflicting_domains_precede_config_load() {
        let tool=create_web_search_tool(std::sync::Arc::new(||panic!("conflicting domains must not load configuration")));
        let result=(tool.execute)(maho_tools::definition::ToolCall{id:"invalid",params:json!({"query":"docs","allowed_domains":["a"],"blocked_domains":["b"]}),signal:Default::default(),context:None,on_update:None}).await.unwrap();
        assert_eq!(result.details.unwrap()["phase"],"error");
    }
    #[tokio::test] async fn native_executor_does_not_shim_unbound_auto_discovery() {
        struct Context;
        impl maho_tools::definition::ToolSessionManager for Context {
            fn session_id(&self)->&str {"test"}
            fn session_file(&self)->Option<&std::path::Path> {None}
        }
        impl maho_tools::definition::ToolContext for Context {
            fn cwd(&self)->&std::path::Path {std::path::Path::new("/tmp")}
            fn model(&self)->Option<&maho_ext_api::Model> {None}
            fn thinking_level(&self)->Option<maho_ext_api::ThinkingLevel> {None}
            fn session_manager(&self)->&dyn maho_tools::definition::ToolSessionManager {self}
            fn goal_store_file(&self)->Option<&std::path::Path> {None}
        }
        let tool=create_web_search_tool(std::sync::Arc::new(||super::super::types::ConfigLoadResult::Ok{config:config(),source:"test".into()}));
        assert!(matches!((tool.execute)(maho_tools::definition::ToolCall{id:"auto",params:json!({"query":"docs"}),signal:Default::default(),context:Some(&Context),on_update:None}).await,Err(maho_tools::definition::ToolError::Message(_))));
    }
    #[tokio::test] async fn aborted_auto_route_without_context_preserves_cancellation() {
        let tool=create_web_search_tool(std::sync::Arc::new(||super::super::types::ConfigLoadResult::Ok{config:config(),source:"test".into()}));
        let signal=maho_tools::definition::AbortSignal::default();signal.abort();
        assert!(matches!((tool.execute)(maho_tools::definition::ToolCall{id:"auto",params:json!({"query":"docs"}),signal,context:None,on_update:None}).await,Err(maho_tools::definition::ToolError::Aborted)));
    }
    #[test] fn search_details_use_source_fields_and_omit_absent_values() {
        let details=super::super::types::SearchDetails{provider:SearchProvider::Exa,entry_id:None,query:"docs".into(),results:vec![super::super::types::SearchResultItem{title:"Docs".into(),url:"https://example.com".into(),snippet:None,source:None,score:Some(0.),published_at:Some("2026-10-02".into())}],duration_ms:2.,truncated:false,strategy:Some(RoutingStrategy::RoundRobin),attempts:Some(vec![]),answer:None,error:None};
        let value=search_details_value(&details); assert_eq!(value["durationMs"],2.); assert_eq!(value["strategy"],"round-robin"); assert_eq!(value["results"][0]["publishedAt"],"2026-10-02"); assert_eq!(value["results"][0]["score"],0.); assert!(value.get("entryId").is_none()); assert!(value["results"][0].get("snippet").is_none()); assert_eq!(value["attempts"],json!([]));
    }
    #[test] fn conflicting_nonempty_lists_fail_before_search() { assert!(request_from_arguments("q".into(),Some(vec!["a".into()]),Some(vec!["b".into()]),&config()).is_err()); assert!(request_from_arguments("q".into(),Some(vec![]),Some(vec!["b".into()]),&config()).is_ok()); }
    #[test] fn routing_state_only_resets_on_identity_or_count_change() { let mut config=config(); let mut key=String::new(); let mut state=None; sync_routing_state(&config,&mut key,&mut state); state.as_mut().unwrap().success_counts[0]=2; config.providers[0].config.model=Some("new".into()); sync_routing_state(&config,&mut key,&mut state); assert_eq!(state.as_ref().unwrap().success_counts,[2]); config.providers[0].config.id=Some("second".into()); sync_routing_state(&config,&mut key,&mut state); assert_eq!(state.unwrap().success_counts,[0]); }
    #[test] fn query_schema_and_default_limit() { assert_eq!(parameters()["properties"]["query"]["minLength"],2); assert_eq!(parameters()["additionalProperties"],false); assert_eq!(request_from_arguments("q".into(),None,None,&config()).unwrap().max_results,10.); }
}
