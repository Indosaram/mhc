use maho_ext_imagegen::auth::*;
use maho_ai::model::Model;
use std::collections::BTreeMap;
struct Registry { stored:bool, models:Vec<Model>, keys:BTreeMap<String,String> }
impl ImageGenAuthRegistry for Registry {
    fn stored_openai_is_api_key(&self)->bool {self.stored}
    fn get_all(&self)->Vec<Model> {self.models.clone()}
    fn get_provider_auth(&self,provider:&str)->AuthFuture<'_> {
        let key=self.keys.get(provider).cloned();
        Box::pin(async move {Ok(key.map(|key|Credentials {api_key:Some(key),headers:BTreeMap::from([("kept".into(),Some(" value ".into())),("empty".into(),Some(" ".into())),("null".into(),None)])}))})
    }
    fn get_api_key_and_headers<'a>(&'a self,model:&'a Model)->AuthFuture<'a> {self.get_provider_auth(&model.provider)}
}
fn model(provider:&str,id:&str)->Model {serde_json::from_value(serde_json::json!({"provider":provider,"id":id,"name":id,"api":"openai-responses","baseUrl":" https://gateway.example/v1 ","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":1024})).expect("model")}
#[tokio::test]
async fn source_auth_priority_headers_and_placeholder_matrix() {
    for (stored,pin,openai,expected,provenance) in [
        (true,None,"synthetic-store",Some("openai"),"store"),
        (false,Some("z-gateway"),"synthetic-store",Some("z-gateway"),"provider-config"),
        (false,Some("missing"),"synthetic-store",Some("a-openai"),"provider-config"),
        (true,None,"SK-SENTINEL-DO-NOT-LOG-2",Some("a-openai"),"provider-config"),
    ] {
        let registry=Registry {stored,models:vec![model("z-gateway","b"),model("a-openai","b"),model("a-openai","a")],keys:BTreeMap::from([("openai".into(),openai.into()),("z-gateway".into(),"synthetic-z".into()),("a-openai".into(),"synthetic-a".into())])};
        let env=pin.map(|pin|BTreeMap::from([("PI_IMAGE_GEN_PROVIDER".into(),pin.into())])).unwrap_or_default();
        let ImageGenAuthResolution::Configured {provider_id,provenance:actual,headers,..}=resolve_image_gen_auth(&registry,&env).await else {panic!("expected auth")};
        assert_eq!(provider_id.as_deref(),expected);assert_eq!(actual,provenance);assert_eq!(headers,BTreeMap::from([("kept".into()," value ".into())]));
    }
}
#[tokio::test]
async fn environment_is_last_resort_and_sentinel_is_not_auth() {
    let registry=Registry {stored:false,models:Vec::new(),keys:BTreeMap::new()};
    for key in [""," ","sk-sentinel-do-not-log-2"] {assert!(matches!(resolve_image_gen_auth(&registry,&BTreeMap::from([("OPENAI_API_KEY".into(),key.into())])).await,ImageGenAuthResolution::None {..}));}
    assert!(matches!(resolve_image_gen_auth(&registry,&BTreeMap::from([("OPENAI_API_KEY".into()," synthetic-env ".into())])).await,ImageGenAuthResolution::Configured {api_key,provider_id:None,provenance:"env",..} if api_key=="synthetic-env"));
}

#[tokio::test]
async fn invalid_routes_never_borrow_credentials_from_another_model() {
    for invalid in ["base","api","key"] {
        let mut broken=model("a-openai","a");
        if invalid=="base" {broken.base_url.clear();}
        if invalid=="api" {broken.api="anthropic-messages".into();}
        let registry=Registry {stored:false,models:vec![broken,model("fallback","b")],keys:BTreeMap::from([("a-openai".into(),if invalid=="key" {" ".into()} else {"synthetic-broken".into()}),("fallback".into(),"synthetic-fallback".into())])};
        let env=BTreeMap::from([("PI_IMAGE_GEN_PROVIDER".into(),"a-openai".into()),("OPENAI_API_KEY".into(),"synthetic-env".into())]);
        assert!(matches!(resolve_image_gen_auth(&registry,&env).await,ImageGenAuthResolution::Configured {provider_id:Some(provider),api_key,..} if provider=="fallback" && api_key=="synthetic-fallback"));
    }
}

struct HeadersOnlyRegistry;
impl ImageGenAuthRegistry for HeadersOnlyRegistry {
    fn stored_openai_is_api_key(&self)->bool {true}
    fn get_all(&self)->Vec<Model> {vec![model("headers-only","a"),model("keyed","a")]}
    fn get_provider_auth(&self,_:&str)->AuthFuture<'_> {Box::pin(async {Err("synthetic unavailable".into())})}
    fn get_api_key_and_headers<'a>(&'a self,model:&'a Model)->AuthFuture<'a> {
        Box::pin(async move {Ok(Some(Credentials {api_key:(model.provider=="keyed").then(||"synthetic-key".into()),headers:BTreeMap::from([("X-Route".into(),Some("route".into()))])}))})
    }
}
#[tokio::test]
async fn headers_only_pin_falls_through_without_synthesizing_authorization() {
    let env=BTreeMap::from([("PI_IMAGE_GEN_PROVIDER".into(),"headers-only".into())]);
    let ImageGenAuthResolution::Configured {provider_id,headers,..}=resolve_image_gen_auth(&HeadersOnlyRegistry,&env).await else {panic!("keyed gateway")};
    assert_eq!(provider_id.as_deref(),Some("keyed"));
    assert_eq!(headers,BTreeMap::from([("X-Route".into(),"route".into())]));
}
