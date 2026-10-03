use maho_ext_api::FlagValue;
use maho_ext_model_fallback::settings::is_model_fallback_disabled;
#[test]
fn flag_and_environment_override(){
    for (flag,environment,expected) in [(Some(FlagValue::Boolean(true)),None,true),(Some(FlagValue::Boolean(false)),Some("1"),true),(None,None,false),(Some(FlagValue::String("true".into())),None,false),(Some(FlagValue::Boolean(false)),Some("true"),false)]{assert_eq!(is_model_fallback_disabled(flag.as_ref(),environment),expected);}
}

struct Settings;
impl maho_ext_api::ExtensionSessionSettings for Settings{
    fn get_retry_fallback_settings(&self)->maho_ext_api::RetryFallbackSettings{maho_ext_api::RetryFallbackSettings{model_fallback:true,chains:std::collections::BTreeMap::from([("target/main".into(),vec!["family".into()])]),revert_policy:maho_ext_api::FallbackRevertPolicy::Never}}
    fn set_fallback_chain<'a>(&'a self,_:&'a str,_:&'a [String])->maho_ext_api::ExtensionFuture<'a,()>{Box::pin(async{panic!("display must not mutate settings")})}
    fn remove_fallback_chain<'a>(&'a self,_:&'a str)->maho_ext_api::ExtensionFuture<'a,()>{Box::pin(async{panic!("display must not mutate settings")})}
    fn set_model_fallback_enabled(&self,_:bool)->maho_ext_api::ExtensionFuture<'_,()>{Box::pin(async{panic!("display must not mutate settings")})}
    fn set_fallback_revert_policy(&self,_:maho_ext_api::FallbackRevertPolicy)->maho_ext_api::ExtensionFuture<'_,()>{Box::pin(async{panic!("display must not mutate settings")})}
    fn reload(&self)->maho_ext_api::ExtensionFuture<'_,()>{Box::pin(async{panic!("display must not reload")})}
    fn get_fallback_status(&self)->Option<maho_ext_api::RetryFallbackStatus>{None}
}
struct Models{all:Vec<maho_ext_api::Model>,available:Vec<maho_ext_api::Model>}
impl maho_ext_api::ModelRegistry for Models{
    fn get_all(&self)->Vec<maho_ext_api::Model>{self.all.clone()}
    fn get_available(&self)->Vec<maho_ext_api::Model>{self.available.clone()}
    fn find(&self,provider:&str,id:&str)->Option<maho_ext_api::Model>{self.all.iter().find(|model|model.provider==provider&&model.id==id).cloned()}
    fn has_configured_auth(&self,_:&maho_ext_api::Model)->bool{true}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->maho_ext_api::ExtensionFuture<'a,Option<String>>{Box::pin(async{panic!("display must not request secrets")})}
}
#[test]
fn display_expansion_uses_effective_model_oauth_and_selectable_models(){
    use maho_ext_model_fallback::settings::load_fallback_settings;
    let model=|provider:&str,id:&str|serde_json::from_value::<maho_ext_api::Model>(serde_json::json!({"provider":provider,"id":id,"name":id,"api":"openai-completions","baseUrl":"https://example.invalid/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":1000,"maxTokens":100})).expect("model");
    let target=model("target","main");let subscription=model("z-oauth","family");let api_key=model("anthropic","family");let unavailable=model("anthropic-subscription","family");
    let registry=Models{all:vec![target.clone(),api_key.clone(),subscription.clone(),unavailable],available:vec![target,api_key,subscription]};
    let queries=std::cell::RefCell::new(Vec::new());
    let oauth=|model:&maho_ext_api::Model|{queries.borrow_mut().push((model.provider.clone(),model.id.clone()));model.provider=="z-oauth"&&model.id=="family"};
    let loaded=load_fallback_settings(&Settings,&registry,&oauth);
    assert_eq!(loaded.chains["target/main"],["z-oauth/family","anthropic/family"]);
    assert!(queries.borrow().iter().any(|(provider,id)|provider=="z-oauth"&&id=="family"));
    assert!(!queries.borrow().iter().any(|(provider,_)|provider=="anthropic-subscription"));
    assert_eq!(loaded.revert_policy,maho_ext_api::FallbackRevertPolicy::Never);
    let registry=Models{available:vec![],..registry};
    let loaded=load_fallback_settings(&Settings,&registry,&oauth);
    assert_eq!(loaded.chains["target/main"][0],"z-oauth/family");
    assert_eq!(loaded.chains["target/main"][1],"anthropic-subscription/family");
}
