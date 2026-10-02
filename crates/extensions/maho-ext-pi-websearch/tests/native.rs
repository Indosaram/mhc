use maho_ext_pi_websearch::websearch::{native::*,types::SearchProvider};
use std::sync::atomic::{AtomicUsize,Ordering};
struct Registry{calls:AtomicUsize}
impl NativeModelRegistry for Registry{
    fn get_api_key_and_headers<'a>(&'a self,_:&'a NativeModelInfo)->NativeAuthFuture<'a>{self.calls.fetch_add(1,Ordering::SeqCst);Box::pin(async{Ok(NativeAuthResult::Success{api_key:Some("native-test".into()),headers:None})})}
}
#[tokio::test]
async fn maps_models_and_preserves_public_id(){
    let registry=Registry{calls:AtomicUsize::new(0)};
    for (provider,id,expected,resource) in [("openai","gpt-5.6-sol",SearchProvider::Openai,"responses"),("anthropic","claude-opus-5",SearchProvider::Anthropic,"messages"),("openrouter","anthropic/claude-opus-5",SearchProvider::Anthropic,"messages"),("zai","glm-5",SearchProvider::Zai,"chat/completions"),("kimi-coding","any",SearchProvider::Kimi,"search")]{
        let model=NativeModelInfo{provider:provider.into(),id:id.into(),base_url:"https://gateway.example.com/v1/#fragment".into()};
        let entry=build_native_entry(Some(&model),Some(&registry),"custom").await.unwrap_or_else(|error|panic!("native entry: {error}")).unwrap_or_else(||panic!("missing native entry"));
        assert_eq!(entry.config.provider,expected);assert_eq!(entry.config.id.as_deref(),Some("custom"));assert_eq!(entry.config.model.as_deref(),Some(id));assert_eq!(entry.config.base_url,Some(format!("https://gateway.example.com/v1/{resource}")));assert_eq!(entry.priority,Some(-1.0));
    }
}
#[tokio::test]
async fn unsafe_and_unsupported_routes_do_not_resolve_auth(){
    let registry=Registry{calls:AtomicUsize::new(0)};
    for base in ["http://127.0.0.1/v1","https://localhost./v1","https://[::1]/v1","https://user:pass@gateway.example.com/v1","not-a-url"]{
        let model=NativeModelInfo{provider:"openai".into(),id:"gpt-5.5".into(),base_url:base.into()};assert!(build_native_entry(Some(&model),Some(&registry),"native").await.unwrap_or_else(|error|panic!("native entry: {error}")).is_none());
    }
    for id in ["gpt-5.3-codex","gpt-4-turbo","o3"]{
        let model=NativeModelInfo{provider:"openai".into(),id:id.into(),base_url:"https://gateway.example.com/v1".into()};assert!(build_native_entry(Some(&model),Some(&registry),"native").await.unwrap_or_else(|error|panic!("native entry: {error}")).is_none());
    }
    assert_eq!(registry.calls.load(Ordering::SeqCst),0);
}
#[tokio::test]
async fn discovery_deduplicates_before_auth_and_keeps_active_first(){
    struct Discovery{calls:AtomicUsize}
    impl NativeModelRegistry for Discovery{
        fn get_api_key_and_headers<'a>(&'a self,model:&'a NativeModelInfo)->NativeAuthFuture<'a>{self.calls.fetch_add(1,Ordering::SeqCst);Box::pin(async move{if model.id=="claude-first"{Ok(NativeAuthResult::Failure{error:"no auth".into()})}else{Ok(NativeAuthResult::Success{api_key:Some("fixture".into()),headers:None})}})}
        fn get_available(&self)->Option<Vec<NativeModelInfo>>{Some([("openai","gpt-5.5"),("anthropic","claude-first"),("anthropic","claude-second"),("xai","grok-4.3")].into_iter().map(|(provider,id)|NativeModelInfo{provider:provider.into(),id:id.into(),base_url:"https://gateway.example.com/v1".into()}).collect())}
    }
    let registry=Discovery{calls:AtomicUsize::new(0)};let model=NativeModelInfo{provider:"openai".into(),id:"gpt-5".into(),base_url:"https://gateway.example.com/v1".into()};
    let entries=build_native_entries(Some(&model),Some(&registry)).await.unwrap_or_else(|error|panic!("discovery: {error}"));
    assert_eq!(entries.len(),2);assert_eq!(entries[0].config.id.as_deref(),Some("native"));assert_eq!(entries[1].config.provider,SearchProvider::Xai);
    let id=entries[1].config.id.as_deref().unwrap_or_else(||panic!("missing discovered ID"));assert!(id.starts_with("native-xai-"));assert_eq!(id.len(),27);assert_eq!(registry.calls.load(Ordering::SeqCst),3);
}
