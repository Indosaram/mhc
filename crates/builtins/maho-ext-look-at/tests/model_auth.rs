use maho_ext_api::{Model,ModelRegistry,ExtensionFuture,ResolvedRequestAuth};
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self)->Vec<Model>{vec![]}
    fn get_available(&self)->Vec<Model>{vec![]}
    fn find(&self,_:&str,_:&str)->Option<Model>{None}
    fn has_configured_auth(&self,_:&Model)->bool{true}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{panic!("provider-only lookup must not be used")})}
    fn get_api_key_and_headers<'a>(&'a self,model:&'a Model)->ExtensionFuture<'a,ResolvedRequestAuth>{Box::pin(async move{
        if model.id=="failed"{return Err("auth denied".into());}
        Ok(ResolvedRequestAuth{auth:maho_ai::models::ProviderAuthResult{api_key:None,headers:Some([("x-model".into(),Some(model.id.clone())),("remove".into(),None)].into()),base_url:Some("https://route.invalid".into())},extra_body:Some([("route".into(),serde_json::json!(model.id))].into_iter().collect()),upstream_model_id:Some("upstream".into()),service_tier:Some(maho_ai::types::ServiceTierPreference::Flex),env:Some([("MODEL".into(),model.id.clone())].into())})
    })}
}
fn model(id:&str)->Model{serde_json::from_value(serde_json::json!({"id":id,"name":id,"provider":"fixture","api":"faux","baseUrl":"","reasoning":false,"input":["text","image"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":1024})).expect("valid model auth fixture")}
#[tokio::test]
async fn preflight_preserves_model_auth_projection_without_provider_fallback(){
    for id in ["first","second"]{
        let auth=maho_ext_look_at::runner::preflight_model_auth(&Registry,&model(id)).await.unwrap();
        assert_eq!(auth.auth.api_key,None);let headers=auth.auth.headers.unwrap();assert_eq!(headers["x-model"].as_deref(),Some(id));assert_eq!(headers["remove"],None);
        assert_eq!(auth.auth.base_url.as_deref(),Some("https://route.invalid"));assert_eq!(auth.extra_body.unwrap()["route"],id);assert_eq!(auth.upstream_model_id.as_deref(),Some("upstream"));assert_eq!(auth.service_tier,Some(maho_ai::types::ServiceTierPreference::Flex));assert_eq!(auth.env.unwrap()["MODEL"],id);
    }
    assert!(maho_ext_look_at::runner::preflight_model_auth(&Registry,&model("failed")).await.is_err());
}
