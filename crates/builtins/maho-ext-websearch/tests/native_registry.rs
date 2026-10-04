use maho_ext_api::{Model,ModelRegistry,ExtensionFuture,ResolvedRequestAuth};
use maho_ext_websearch::websearch::{native::config_with_native_routes,types::*};
struct Registry{models:Vec<Model>,calls:std::sync::Mutex<Vec<String>>}
impl ModelRegistry for Registry{
    fn get_all(&self)->Vec<Model>{self.models.clone()}
    fn get_available(&self)->Vec<Model>{self.get_all()}
    fn find(&self,_:&str,_:&str)->Option<Model>{None}
    fn has_configured_auth(&self,_:&Model)->bool{true}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{panic!("provider fallback forbidden")})}
    fn get_api_key_and_headers<'a>(&'a self,model:&'a Model)->ExtensionFuture<'a,ResolvedRequestAuth>{Box::pin(async move{
        self.calls.lock().expect("auth call recorder lock").push(model.id.clone());
        if model.id=="gpt-5-denied"{return Err("denied".into());}
        Ok(ResolvedRequestAuth{auth:maho_ai::models::ProviderAuthResult{api_key:Some(model.id.clone()),headers:None,base_url:None},extra_body:None,upstream_model_id:None,service_tier:None,env:None})
    })}
}
fn model(id:&str,host:&str)->Model{serde_json::from_value(serde_json::json!({"id":id,"name":id,"provider":"openai","api":"openai-responses","baseUrl":format!("https://{host}/v1"),"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":1024})).expect("valid native discovery model fixture")}
#[tokio::test]
async fn auto_routes_use_full_models_skip_failed_seen_route_and_prepend_available_order(){
    let active=model("gpt-5-denied","active.example.com");
    let registry=std::sync::Arc::new(Registry{models:vec![model("gpt-5-duplicate","active.example.com"),model("gpt-5-second","second.example.com"),model("gpt-5-third","third.example.com")],calls:Default::default()});
    let live:std::sync::Arc<dyn ModelRegistry>=registry.clone();let mut configured=SearchProviderConfig::new(SearchProvider::Exa);configured.max_results=Some(7.);
    let config=WebsearchConfig{strategy:RoutingStrategy::Priority,fallback:true,auto:true,providers:vec![SearchProviderEntry{config:configured,priority:None,weight:None}]};
    let merged=config_with_native_routes(config,Some(&active),Some(&live),None).await.unwrap();
    assert_eq!(*registry.calls.lock().unwrap(),["gpt-5-denied","gpt-5-second","gpt-5-third"]);
    assert_eq!(merged.providers.len(),3);assert_eq!(merged.providers[0].config.model.as_deref(),Some("gpt-5-second"));assert_eq!(merged.providers[1].config.model.as_deref(),Some("gpt-5-third"));assert_eq!(merged.providers[2].config.max_results,Some(7.));
}
#[tokio::test]
async fn automatic_auth_failure_preserves_configured_http_search_and_request_limit(){
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("HTTP fixture listener");let address=listener.local_addr().expect("listener address");
    let active=model("gpt-5-denied","active.example.com");let registry=std::sync::Arc::new(Registry{models:vec![],calls:Default::default()});let live:std::sync::Arc<dyn ModelRegistry>=registry.clone();
    let mut provider=SearchProviderConfig::new(SearchProvider::Exa);provider.base_url=Some(format!("http://{address}/search"));provider.api_key=Some("fixture".into());provider.max_results=Some(7.);
    let config=WebsearchConfig{auto:true,fallback:true,strategy:RoutingStrategy::Priority,providers:vec![SearchProviderEntry{config:provider,priority:None,weight:None}]};
    let tool=maho_ext_websearch::websearch::tool::create_web_search_tool_with_registry(std::sync::Arc::new(move ||ConfigLoadResult::Ok{config:config.clone(),source:"HTTP fixture".into()}),Some(active),Some(live),Default::default());
    let server=async{
        let (mut socket,_)=listener.accept().await.expect("search connection");let mut bytes=Vec::new();let mut chunk=[0;4096];
        let header_end=loop{let count=socket.read(&mut chunk).await.expect("request bytes");assert!(count>0);bytes.extend_from_slice(&chunk[..count]);if let Some(end)=bytes.windows(4).position(|part|part==b"\r\n\r\n"){break end+4;}};
        let header=String::from_utf8_lossy(&bytes[..header_end]);let length=header.lines().find_map(|line|line.to_lowercase().strip_prefix("content-length:").map(|value|value.trim().parse::<usize>().expect("content length"))).expect("body length");
        while bytes.len()<header_end+length{let count=socket.read(&mut chunk).await.expect("body bytes");assert!(count>0);bytes.extend_from_slice(&chunk[..count]);}
        let body:serde_json::Value=serde_json::from_slice(&bytes[header_end..header_end+length]).expect("search payload");assert_eq!(body["numResults"].as_f64(),Some(7.));
        let response=r#"{"results":[{"title":"Configured search","url":"https://example.com/result","text":"fixture"}]}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.expect("response");
    };
    let client=async{(tool.execute)(maho_tools::definition::ToolCall{id:"auto-http",params:serde_json::json!({"query":"configured fallback"}),signal:Default::default(),context:None,on_update:None}).await.expect("automatic search")};
    let ((),result)=tokio::time::timeout(std::time::Duration::from_secs(5),async{tokio::join!(server,client)}).await.expect("HTTP signal completion");
    assert_eq!(*registry.calls.lock().expect("auth calls"),["gpt-5-denied"]);assert_eq!(result.details.expect("search details")["results"][0]["title"],"Configured search");
}
