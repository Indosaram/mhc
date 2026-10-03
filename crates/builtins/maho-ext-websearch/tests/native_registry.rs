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
